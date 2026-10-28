use std::{
    ffi::OsStr,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{Duration, Instant},
};
use supercov_engine::fault_fs as fs;
use supercov_engine::fault_process::Launch;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use supercov_engine::{
    agent_json,
    coverage_index::CoverageIndex,
    coverage_report::{ArchiveReportRequest, ReportError, analyze_coverage_archive},
    indexed_query::{
        IndexedQueryOutput, IndexedQueryRequest, NewerQuery, execute_indexed_query, query_indexed,
    },
    js_instrumenter::instrument_candidate,
    run_query::{RunListData, run_list_query},
    run_store::{
        RunIndexError, RunInventory, RunStoreError, StoredRun, compare_run_integrity,
        discover_runs, open_or_rebuild_query_index, select_run,
    },
};
use time::{OffsetDateTime, macros::format_description};

// Standard output and error that cannot be written are not Supercov's
// failure: `supercov -- npm test 2>&1 | head` closes them early, and std's
// printing macros panic then. These write where they do and pass over a
// write that fails.
macro_rules! println {
    ($($argument:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stdout(), $($argument)*);
    }};
}
macro_rules! print {
    ($($argument:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stdout(), $($argument)*);
    }};
}
macro_rules! eprintln {
    ($($argument:tt)*) => {{
        use std::io::Write as _;
        let _ = writeln!(std::io::stderr(), $($argument)*);
    }};
}
macro_rules! eprint {
    ($($argument:tt)*) => {{
        use std::io::Write as _;
        let _ = write!(std::io::stderr(), $($argument)*);
    }};
}

mod assertions_command;
mod gaps_export;
mod html_report;
mod human_query;
mod public_query;
mod quality;
mod source_query;
mod tests_query;

use human_query::render_human;
use public_query::{PublicQueryInvocation, help_for, parse_public_query};
use supercov_engine::progress::ProgressLine;

const HELP: &str = r#"Coverage, security and code quality for coding agents.

Score the source with Jev:
  supercov quality                     score this repository
  supercov quality gaps                only files something fired on
  supercov quality file <path>         one file, every check
  supercov quality patch               what a change introduced
  supercov quality --help              every quality query
  supercov security                    security surface, file by file
  supercov security patch              what a change introduced, security only

Measure your full test command:
  npx supercov -- npm test
  supercov -- <test command>           measure the complete command
  supercov --exact-attribution -- ...  credit every test, running yours in order

Inspect a run with small, paginated answers:
  supercov runs latest                 newest run summary
  supercov runs latest gaps            files with unresolved obligations
  supercov runs latest gaps --kind e2e E2E gaps by origin
  supercov runs latest file <path>     gap lines in one file
  supercov runs <id> --help            every run query
  supercov runs <run-id> [resource]    query one immutable run

Check what the tests assert:
  supercov runs latest assertions         how much executed code the tests assert
  supercov runs latest assertions assess  work it out with Jev (TYPESAFE_API_KEY)

Compare, combine, and maintain:
  supercov diff <older> <newer>        compare two runs
  supercov report [run-id]             open a portable interactive HTML report
  supercov merge [<id> <id> ...]       combine runs; with none, those on the current source
  supercov import <tracefile> --test <name>
                                       add line evidence measured where Supercov could not run
  supercov runs clean [--keep N]       remove stored runs (all by default)
  supercov quality clean [--keep N]    remove saved assessments

Settings, in the environment of the measured command:
  SUPERCOV_SOURCE_ROOTS=src,app        measure only these directories or files
  SUPERCOV_TEST_KIND=e2e               the test level of the command's tests
  SUPERCOV_RUN_ID_FILE=<file>          write the recorded run's id to a file
  SUPERCOV_SOURCE_TOOLS=<names>        run these linters on your files, not the copy
  SUPERCOV_KEEP_WORKSPACE=1            keep the instrumented copy after the run
  SUPERCOV_COMMAND_TIMEOUT_MS=<ms>     stop the test command after this long
  supercov docs cli                    what each does

Guides:
  supercov docs                        list bundled guides
  supercov docs <topic>                print Markdown to stdout
"#;

/// The bundled guides, by topic, in the order `docs` lists them.
const DOC_TOPICS: &[(&str, &str)] = &[
    (
        "getting-started",
        include_str!("../assets/docs/getting-started.md"),
    ),
    ("agent-loop", include_str!("../assets/docs/agent-loop.md")),
    ("assertions", include_str!("../assets/docs/assertions.md")),
    (
        "troubleshooting",
        include_str!("../assets/docs/troubleshooting.md"),
    ),
    ("cli", include_str!("../assets/docs/cli.md")),
    (
        "coverage-model",
        include_str!("../assets/docs/coverage-model.md"),
    ),
    ("quality", include_str!("../assets/docs/quality.md")),
    ("security", include_str!("../assets/docs/security.md")),
    ("evidence", include_str!("../assets/docs/evidence.md")),
    (
        "supported-suites",
        include_str!("../assets/docs/supported-suites.md"),
    ),
    (
        "verification",
        include_str!("../assets/docs/verification.md"),
    ),
    ("performance", include_str!("../assets/docs/performance.md")),
    ("reports", include_str!("../assets/docs/reports.md")),
    (
        "workspace-isolation",
        include_str!("../assets/docs/workspace-isolation.md"),
    ),
];

/// The source files a query is about: a line's, several lines', a file's or
/// a decision's. Empty for a query about the whole run.
fn queried_files(request: &supercov_engine::indexed_query::IndexedQueryRequest) -> Vec<String> {
    let mut files = Vec::new();
    // `file` reaches here as the view it asked for.
    if matches!(
        request.command.as_str(),
        "line" | "file-detail" | "file-decisions" | "decision" | "source"
    ) {
        if let Some(file) = &request.file {
            files.push(file.clone());
        }
        files.extend(request.more_lines.iter().map(|(file, _)| file.clone()));
        // `decision` always has its selector: an id, or a file and line.
        if request.command == "decision"
            && let Some((file, line)) = request
                .selector
                .as_deref()
                .and_then(|selector| selector.rsplit_once(':'))
            && line.parse::<usize>().is_ok()
        {
            files.push(file.to_owned());
        }
    }
    files.sort();
    files.dedup();
    files
}

/// Whether `file` differs from what the run recorded of it, by content; None
/// when the run recorded nothing of it. A run is stale as a whole when any
/// file changed -- a test an agent just added -- but a line of a file that
/// did not change reads exactly as the run measured it.
fn file_changed_since_run(
    root: &Path,
    recorded: Option<&supercov_engine::source_manifest::FileManifest>,
    file: &str,
) -> Option<bool> {
    let was = recorded?.get(file)?;
    let now = fs::read_to_string(root.join(file))
        .ok()
        .map(|text| supercov_engine::source_manifest::FileFingerprint::of(&text).sha256);
    Some(now.as_deref() != Some(was.sha256.as_str()))
}

fn hydrate_line_source_from_working_tree(
    root: &Path,
    recorded: Option<&supercov_engine::source_manifest::FileManifest>,
    data: &mut supercov_engine::coverage_query::CoverageCoversData,
) {
    let (location, source, origin) = match data {
        supercov_engine::coverage_query::CoverageCoversData::Line(data) => {
            (&data.location, &mut data.source, &mut data.source_origin)
        }
        supercov_engine::coverage_query::CoverageCoversData::Anchors(data) => {
            (&data.location, &mut data.source, &mut data.source_origin)
        }
    };
    if source.is_some() {
        return;
    }
    // The query answers only for a file the run recorded, which is always
    // a path within the project; one removed since, or cut short, has no
    // text to give.
    let Some(line) = fs::read_to_string(root.join(&location.file))
        .ok()
        .and_then(|contents| {
            contents
                .lines()
                .nth(location.line.saturating_sub(1))
                .map(str::to_owned)
        })
    else {
        return;
    };
    let changed = file_changed_since_run(root, recorded, &location.file);
    *source = Some(line);
    *origin = Some(
        if changed == Some(true) {
            "working-tree-stale"
        } else {
            "working-tree-current"
        }
        .into(),
    );
}

fn main() -> ExitCode {
    let os_arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    // nextest's target runner for the owned frontend: test arguments pass
    // through untouched, so dispatch before anything reads them as UTF-8.
    if os_arguments.first().is_some_and(|argument| {
        argument == supercov_engine::rust_owned_nextest::RUNNER_MODE_ARGUMENT
    }) {
        return ExitCode::from(
            supercov_engine::rust_owned_nextest::nextest_runner(
                os_arguments.into_iter().skip(1).collect(),
            )
            .clamp(0, 255) as u8,
        );
    }
    // The `wrangler` a measured test starts, and the poller it leaves behind:
    // the test's arguments pass through untouched.
    if let Some(command) = os_arguments.first().and_then(|argument| argument.to_str()) {
        let rest = || os_arguments.iter().skip(1).cloned().collect::<Vec<_>>();
        if command == supercov_engine::rust_worker::WRANGLER_COMMAND {
            return ExitCode::from(
                supercov_engine::rust_worker::launch_wrangler(rest()).clamp(0, 255) as u8,
            );
        }
        if command == supercov_engine::rust_worker::WRANGLER_POLL_COMMAND {
            return ExitCode::from(
                supercov_engine::rust_worker::poll_wrangler(rest()).clamp(0, 255) as u8,
            );
        }
    }
    // The parent-death watchdog inherits arbitrary wrapper environments. It
    // must dispatch before rustdoc wrapper detection and must never emit
    // user-visible output.
    if os_arguments.as_slice() == [OsStr::new("__watch-process-group")] {
        return match supercov_engine::process_supervision::watch_parent_process_group() {
            Ok(()) => ExitCode::SUCCESS,
            Err(_) => ExitCode::from(125),
        };
    }
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    // Doctests of the owned frontend: a merged harness spawns this program
    // once per case; Cargo runs it in place of rustdoc during `cargo test
    // --doc`. Both are told apart by their environment alone.
    if let Some(children) = supercov_engine::rust_owned_doctests::doctest_child_directory() {
        return ExitCode::from(
            supercov_engine::rust_owned_doctests::doctest_child(children).clamp(0, 255) as u8,
        );
    }
    if let Some(root) = supercov_engine::rust_owned_doctests::rustdoc_wrapper_root(&arguments) {
        return ExitCode::from(
            supercov_engine::rust_owned_doctests::rustdoc_wrapper(root, arguments).clamp(0, 255)
                as u8,
        );
    }
    let mut arguments = arguments.into_iter();
    match arguments.next().as_deref() {
        None | Some("--help" | "-h") => {
            print!("{HELP}");
            ExitCode::SUCCESS
        }
        // `help runs` is `runs --help`, as `git help` and `cargo help` read it.
        Some("help") => {
            let asked = vec!["--help".to_owned()];
            match arguments.next().as_deref() {
                None => {
                    print!("{HELP}");
                    ExitCode::SUCCESS
                }
                Some("quality") => quality::command(asked),
                Some("security") => quality::security_command(asked),
                Some("docs") => docs_command(asked),
                Some("runs") => public_query_command("runs", asked),
                Some("diff") => public_query_command("diff", asked),
                Some("report") => html_report::report_command(asked),
                Some("merge") => merge_command(asked),
                Some("import") => import_command(asked),
                Some(other) => {
                    eprintln!(
                        "[supercov] No help for {other}: it is no command. Try supercov help."
                    );
                    ExitCode::from(2)
                }
            }
        }
        Some("--version" | "-V") => {
            println!(
                "supercov {} (rust contract v{})",
                supercov_engine::version(),
                supercov_contracts::CONTRACT_VERSION
            );
            ExitCode::SUCCESS
        }
        Some("--") => public_coverage_run(arguments.collect(), RunOptions::default()),
        // The one thing worth asking for before the command: credit every test
        // with what it reached, wherever that can only be had by running the
        // suite differently. Written once here rather than as a flag per
        // language, because what someone wants is the same thing in all of
        // them.
        Some("--exact-attribution") => match arguments.next().as_deref() {
            Some("--") => public_coverage_run(
                arguments.collect(),
                RunOptions {
                    exact_attribution: true,
                },
            ),
            _ => {
                eprintln!(
                    "[supercov] --exact-attribution goes before the command: supercov --exact-attribution -- <test command>"
                );
                ExitCode::from(2)
            }
        },
        Some("__instrument-js") => instrument_js(),
        Some("__query-stored-run") => query_stored_run(),
        Some("__supervise") => supervise(),
        Some("__run-js-direct") => run_js_direct(),
        Some("__sweep-trash") => sweep_trash(),
        Some(supercov_engine::source_tools::SOURCE_TOOL_COMMAND) => source_tool(),
        Some("__benchmark-js-transform") => benchmark_js_transform(),
        Some(supercov_engine::rust_owned_doctests::RUNTOOL_MODE_ARGUMENT) => ExitCode::from(
            supercov_engine::rust_owned_doctests::doctest_runtool(arguments.collect()).clamp(0, 255)
                as u8,
        ),
        Some("quality") => quality::command(arguments.collect()),
        Some("security") => quality::security_command(arguments.collect()),
        Some("docs") => docs_command(arguments.collect()),
        Some("runs") => {
            let arguments: Vec<String> = arguments.collect();
            match arguments.split_first() {
                Some((first, rest)) if first == "clean" => cleanup_command(rest.to_vec()),
                _ => public_query_command("runs", arguments),
            }
        }
        Some("diff") => public_query_command("diff", arguments.collect()),
        Some("report") => html_report::report_command(arguments.collect()),
        Some("merge") => merge_command(arguments.collect()),
        Some("import") => import_command(arguments.collect()),
        Some(command) => {
            eprintln!("[supercov] Unknown command: {command}. Try supercov help.");
            ExitCode::from(2)
        }
    }
}

/// A mistake in what was asked: under `--json` the failure envelope on
/// standard output, as every answer to `--json` is; otherwise the message, and
/// the usage `help` when there is one, on standard error.
pub(crate) fn refuse(command: &str, json: bool, message: String, help: &str) -> ExitCode {
    if json {
        print!(
            "{}",
            agent_json::failure(
                Some(command),
                &agent_json::AgentError {
                    code: agent_json::ErrorCode::InvalidArgument,
                    message,
                    retryable: false,
                    details: None,
                }
            )
        );
    } else if help.is_empty() {
        eprintln!("[supercov] {message}");
    } else {
        eprint!("[supercov] {message}\n\n{help}");
    }
    ExitCode::from(2)
}

/// The command the reader used to start Supercov. The npm package sets
/// `SUPERCOV_PACKAGE_ROOT`, the Go launcher sets `SUPERCOV_LAUNCHER=go`, and
/// the PyPI, Ruby and Rust packages run the binary itself as `supercov`.
pub(crate) fn launcher_command() -> &'static str {
    match std::env::var("SUPERCOV_LAUNCHER").as_deref() {
        Ok("go") => "go run github.com/supercorp-ai/supercov/cmd/supercov@latest",
        _ if std::env::var_os("SUPERCOV_PACKAGE_ROOT").is_some() => "npx supercov",
        _ => "supercov",
    }
}

/// Guides write commands as `npx supercov …`, and sometimes as bare
/// `supercov …`. Print both the way the reader started Supercov, in shell
/// blocks and inline code, so a pasted command runs. `text` blocks are
/// recorded output and stay exactly as recorded.
fn localize_guide(markdown: &str, command: &str) -> String {
    // A bare `supercov` on its own names the binary; with arguments it is a command.
    let rewrite = |code: &str| -> Option<String> {
        if let Some(rest) = code.strip_prefix("npx supercov") {
            return (rest.is_empty() || rest.starts_with(' ')).then(|| format!("{command}{rest}"));
        }
        let rest = code.strip_prefix("supercov")?;
        rest.starts_with(' ').then(|| format!("{command}{rest}"))
    };
    let mut fence: Option<bool> = None;
    let mut out = String::with_capacity(markdown.len());
    for line in markdown.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            fence = match fence {
                Some(_) => None,
                None => {
                    let info = trimmed.trim_start_matches('`').trim();
                    let language = info.split_whitespace().next().unwrap_or("");
                    Some(matches!(language, "sh" | "bash" | "shell" | "console"))
                }
            };
            out.push_str(line);
            continue;
        }
        match fence {
            Some(true) => {
                let indent = &line[..line.len() - trimmed.len()];
                let (prompt, body) = trimmed
                    .strip_prefix("$ ")
                    .map_or(("", trimmed), |body| ("$ ", body));
                match rewrite(body) {
                    Some(text) => out.push_str(&format!("{indent}{prompt}{text}")),
                    None => out.push_str(line),
                }
            }
            Some(false) => out.push_str(line),
            None => {
                // Inline code: every other backtick-separated piece is code.
                for (index, piece) in line.split('`').enumerate() {
                    if index > 0 {
                        out.push('`');
                    }
                    match (index % 2 == 1).then(|| rewrite(piece)).flatten() {
                        Some(text) => out.push_str(&text),
                        None => out.push_str(piece),
                    }
                }
            }
        }
    }
    out
}

fn docs_command(arguments: Vec<String>) -> ExitCode {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!(
            "Usage: supercov docs [topic]\n\nWithout a topic, lists bundled Markdown guides. With a topic, prints that guide to stdout."
        );
        return ExitCode::SUCCESS;
    }
    if arguments.len() > 1 {
        eprintln!("[supercov] docs accepts at most one topic");
        return ExitCode::from(2);
    }
    let Some(topic) = arguments.first() else {
        println!("Bundled guides");
        for (topic, _) in DOC_TOPICS {
            println!("  {topic}");
        }
        return ExitCode::SUCCESS;
    };
    let Some((_, guide)) = DOC_TOPICS.iter().find(|(name, _)| name == topic) else {
        eprintln!(
            "[supercov] unknown guide {topic:?}; run `{} docs` to list topics",
            launcher_command()
        );
        return ExitCode::from(2);
    };
    print!("{}", localize_guide(guide, launcher_command()));
    ExitCode::SUCCESS
}

fn merge_command(arguments: Vec<String>) -> ExitCode {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!("{MERGE_USAGE}");
        return ExitCode::SUCCESS;
    }
    let allow_changed = arguments
        .iter()
        .any(|argument| argument == "--allow-changed");
    let run_ids = arguments
        .into_iter()
        .filter(|argument| argument != "--allow-changed")
        .collect::<Vec<_>>();
    // An option merge does not take is no run id.
    if let Some(option) = run_ids
        .iter()
        .find(|argument| argument.starts_with('-') && argument.len() > 1)
    {
        eprintln!("[supercov] Unknown option: {option}\n\n{MERGE_USAGE}");
        return ExitCode::from(2);
    }
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    // With no run named, the runs made on the source as it is now: what a
    // script that ran its suites one by one, or side by side, has to add up
    // without knowing what any of them was called.
    let groups = if run_ids.is_empty() {
        match runs_on_this_source(&root, allow_changed) {
            Ok(groups) => groups,
            Err(error) => {
                eprintln!("[supercov] {error}");
                return ExitCode::from(2);
            }
        }
    } else {
        vec![run_ids]
    };
    for run_ids in groups {
        let (run_id, started_at) = public_run_identity();
        match supercov_engine::run_merge::merge_coverage_runs(
            &root,
            &run_ids,
            &run_id,
            &started_at,
            allow_changed,
        ) {
            Ok(merged) => {
                println!("[supercov] merged run {merged}");
                println!("{} runs {merged}", launcher_command());
            }
            Err(error) => {
                eprintln!("[supercov] {error}");
                return ExitCode::from(2);
            }
        }
    }
    ExitCode::SUCCESS
}

const IMPORT_USAGE: &str = "Usage: supercov import <tracefile> (--test <name> | --background) [--run <run-id>]\n                       [--status passed|failed]\n\nAdds line evidence measured where Supercov could not run, as an LCOV tracefile, to a run, as a\nnew run. The lines are credited to a test of their own, or to the run's background, and only by\nline: the evidence says nothing of branches, decisions or which statement on a line ran. A file\nthe run did not measure joins it with one statement per recorded line. A profile another tool\nrecorded is exported as LCOV by that tool first: `supercov docs cli` shows how.\n\n--test <name>     credit the lines to a test with this name (kind imported, runner lines)\n--background      credit them to the run's background\n--run <run-id>    the run to add them to (default: the latest)\n--status <status> the imported test's outcome, where it is known";

fn import_command(arguments: Vec<String>) -> ExitCode {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        println!("{IMPORT_USAGE}");
        return ExitCode::SUCCESS;
    }
    let mut evidence = None;
    let mut test = None;
    let mut background = false;
    let mut run = None;
    let mut status = None;
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        let (flag, inline) = match argument.split_once('=') {
            Some((flag, value)) if flag.starts_with("--") => {
                (flag.to_owned(), Some(value.to_owned()))
            }
            _ => (argument.clone(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| arguments.next())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        let parsed = match flag.as_str() {
            "--test" => value("--test").map(|name| test = Some(name)),
            "--background" => {
                background = true;
                Ok(())
            }
            "--run" => value("--run").map(|id| run = Some(id)),
            "--status" => value("--status").map(|value| status = Some(value)),
            _ if argument.starts_with('-') => Err(format!("unknown option {argument}")),
            _ if evidence.is_none() => {
                evidence = Some(PathBuf::from(argument));
                Ok(())
            }
            _ => Err(format!("unexpected argument {argument}")),
        };
        if let Err(error) = parsed {
            eprintln!("[supercov] {error}\n\n{IMPORT_USAGE}");
            return ExitCode::from(2);
        }
    }
    let credit = match (test, background) {
        (Some(name), false) => supercov_engine::outside_evidence::Credit::Test(name),
        (None, true) => supercov_engine::outside_evidence::Credit::Background,
        (Some(_), true) => {
            eprintln!(
                "[supercov] --test and --background each name who the lines are credited to; give one\n\n{IMPORT_USAGE}"
            );
            return ExitCode::from(2);
        }
        (None, false) => {
            eprintln!(
                "[supercov] name who the lines are credited to: --test <name> or --background\n\n{IMPORT_USAGE}"
            );
            return ExitCode::from(2);
        }
    };
    let Some(evidence) = evidence else {
        eprintln!("[supercov] name the evidence file to import\n\n{IMPORT_USAGE}");
        return ExitCode::from(2);
    };
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let (run_id, started_at) = public_run_identity();
    match supercov_engine::outside_evidence::import_line_evidence(
        &supercov_engine::outside_evidence::ImportRequest {
            root: &root,
            run: run.as_deref(),
            evidence: &evidence,
            credit,
            status: status.as_deref(),
            run_id: &run_id,
            started_at: &started_at,
        },
    ) {
        Ok(outcome) => {
            println!(
                "[supercov] imported {} line(s) that ran into run {} as run {}: {} statement(s) and function(s) credited, by line only",
                outcome.lines_run, outcome.parent, outcome.run_id, outcome.credited
            );
            if !outcome.added_files.is_empty() {
                println!(
                    "[supercov] measured only by this evidence: {}",
                    supercov_engine::run_merge::listed(&outcome.added_files)
                );
            }
            if !outcome.skipped.is_empty() {
                eprintln!(
                    "[supercov] left out {} file(s) of the evidence: {}",
                    outcome.skipped.len(),
                    supercov_engine::run_merge::listed(
                        &outcome
                            .skipped
                            .iter()
                            .map(|(file, reason)| format!("{file} ({reason})"))
                            .collect::<Vec<_>>()
                    )
                );
            }
            println!("{} runs {}", launcher_command(), outcome.run_id);
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

/// What a recorded run says when it is done: where its evidence is, and,
/// for a script that has to name it, its id in the file
/// `SUPERCOV_RUN_ID_FILE` names. Guessing the newest run is wrong when two
/// commands record runs side by side.
#[track_caller]
fn published(run_directory: &Path) {
    println!(
        "[coverage] evidence: {}",
        run_directory.join("evidence.raw.gz").display()
    );
    if let (Some(file), Some(id)) = (
        std::env::var_os(RUN_ID_FILE).filter(|file| !file.is_empty()),
        run_directory.file_name().and_then(|id| id.to_str()),
    ) && let Err(error) = fs::write(&file, format!("{id}\n"))
    {
        eprintln!(
            "[supercov] could not write the run id to {}: {error}",
            Path::new(&file).display()
        );
    }
}

/// Why a measured command failed, as far as its run can say. A test that
/// failed is named; only when none did -- the command stopped before its
/// tests, or a test file never loaded -- is the build output the copy starts
/// without the likely cause, and said to be.
fn explain_failed_command(
    root: &Path,
    run_id: &str,
    exit_code: i32,
    unrun_build: &[String],
    command: &str,
) {
    let failed = discover_runs(root)
        .ok()
        .and_then(|inventory| {
            let run = select_run(&inventory, Some(run_id)).ok()?;
            let container = open_or_rebuild_query_index(run).ok()?;
            let index = CoverageIndex::new(&container).ok()?;
            index
                .test_summaries(supercov_engine::coverage_index::CoverageViewId::All)
                .ok()
        })
        .unwrap_or_default()
        .into_iter()
        .filter(|test| {
            test.role == "test" && matches!(test.outcome.as_str(), "failed" | "timedOut")
        })
        .map(|test| test.name)
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        let shown = failed
            .iter()
            .take(5)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let more = failed.len().saturating_sub(5);
        eprintln!(
            "[supercov] the command exited {exit_code}: {} test(s) failed: {shown}{}",
            failed.len(),
            if more > 0 {
                format!(", and {more} more")
            } else {
                String::new()
            }
        );
    } else if !unrun_build.is_empty() {
        let build = unrun_build.join(" ");
        eprintln!(
            "[supercov] the command exited {exit_code} and no test it reported failed. It ran in a copy of the project that starts without build output, and Supercov runs only the command it is given: if these tests need `{build}` first, put it in the command, as in `supercov -- sh -c \"{build} && {command}\"`"
        );
    }
}

/// Names the file a run writes its id to.
const RUN_ID_FILE: &str = "SUPERCOV_RUN_ID_FILE";

const MERGE_USAGE: &str = "Usage: supercov merge [--allow-changed] [<run-id> <run-id> ...]\n\nWith no run named, merges the runs recorded on the source as it is now: the newest of each\ncommand, leaving out runs that are themselves a merge.\n\n--allow-changed  also merge runs recorded before files the tests load (helpers, fixtures)\n                 or the test configuration changed. The product code and what is measured\n                 must still be the same; the merged run names what differed.";

/// The runs to merge when none is named, newest first, one list for each
/// set that can be one run.
///
/// A run counts when the checkout has not moved on from it, which is the
/// condition merging puts on its inputs anyway. Of a command run more than
/// once only the newest is taken: a test that failed and then passed is
/// reported failed by a merge of both, so a suite run again must replace its
/// first attempt. Newest first, because where two runs hold one test the
/// first named decides its kind.
fn runs_on_this_source(root: &Path, allow_changed: bool) -> Result<Vec<Vec<String>>, String> {
    let inventory = public_run_inventory(root).map_err(|error| error.to_string())?;
    // A run of this source whose tests have changed since, with the first
    // of its test files that is not as it was: said, so that a run left out
    // is not a run silently missing from the total.
    let mut outdated = Vec::new();
    // Every file known to hold tests: one of another test changing takes
    // nothing from a run that test was not in.
    let tests = inventory
        .runs
        .iter()
        .filter_map(supercov_engine::source_manifest::test_files)
        .flatten()
        .collect::<std::collections::BTreeSet<_>>();
    let checkout = supercov_engine::run_currency::Checkout::new(root, &tests);
    // How Playwright resolves now: as the newest run that ran with the
    // configuration as it is did. Asked only when a run's Playwright
    // configuration file has changed since.
    let reference = std::cell::OnceCell::new();
    let reference = || {
        reference
            .get_or_init(|| {
                let mut runs = inventory.runs.iter().collect::<Vec<_>>();
                runs.sort_by(|left, right| {
                    right.metadata.started_at.cmp(&left.metadata.started_at)
                });
                runs.into_iter()
                    .filter(|run| run.metadata.merged != Some(true))
                    .filter(|run| {
                        current_integrity_for_run(root, run).is_some_and(|current| {
                            run.metadata.integrity.fingerprint.configuration
                                == current.fingerprint.configuration
                        })
                    })
                    .find_map(|run| supercov_engine::run_currency::Ran::of(run).playwright)
            })
            .clone()
    };
    let mut current = inventory
        .runs
        .iter()
        .filter(|run| run.metadata.merged != Some(true))
        .filter(|run| {
            // Said like any other run left out: a source that cannot be
            // fingerprinted now (its last measured file deleted) is still a
            // run missing from the total.
            let Some(current) = current_integrity_for_run(root, run) else {
                outdated.push((
                    *run,
                    "its source cannot be measured as it is now".to_owned(),
                    true,
                ));
                return false;
            };
            let recorded = &run.metadata.integrity;
            // A run of another release reads as current -- its checkout
            // has not moved -- but cannot be merged with this release's.
            if recorded.fingerprint.instrumenter != current.fingerprint.instrumenter {
                outdated.push((
                    *run,
                    supercov_engine::run_merge::recorded_by_another_release(run),
                    true,
                ));
                return false;
            }
            if !compare_run_integrity(Some(recorded), &current).stale {
                return true;
            }
            let files = supercov_engine::fault::or(
                "manifest-unreadable",
                || None,
                || supercov_engine::source_manifest::load_manifest(run).ok(),
            );
            // What is measured has moved on: nothing makes this run a
            // record of it. Said, with the files, so that a run left out
            // is not a run silently missing from the total.
            let moved = |domain: &str| {
                let named = files
                    .as_ref()
                    .map(|files| {
                        supercov_engine::source_manifest::changed_files(
                            &files.manifest.files,
                            |file| {
                                fs::read_to_string(root.join(file)).ok().map(|text| {
                                    supercov_engine::source_manifest::FileFingerprint::of(&text)
                                        .sha256
                                })
                            },
                        )
                        .into_iter()
                        .filter(|file| {
                            !tests.contains(file)
                                && !supercov_engine::run_currency::configuration_file(file)
                        })
                        .take(3)
                        .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                // An added file is in no record to compare against, so
                // it is counted rather than named.
                let added = current
                    .fingerprint
                    .source_files
                    .saturating_sub(recorded.fingerprint.source_files);
                if named.is_empty() && added > 0 {
                    format!("the {domain} has changed ({added} file(s) added)")
                } else if named.is_empty() {
                    format!("the {domain} has changed")
                } else {
                    format!("the {domain} has changed ({})", named.join(", "))
                }
            };
            // A measured file added or removed: what is measured differs.
            // A change to a file is judged below, file by file.
            if recorded.fingerprint.source != current.fingerprint.source
                && (files.is_none()
                    || recorded.fingerprint.source_files != current.fingerprint.source_files)
            {
                outdated.push((*run, moved("source"), true));
                return false;
            }
            if recorded.fingerprint.dependencies != current.fingerprint.dependencies {
                outdated.push((*run, "the dependencies have changed".to_owned(), true));
                return false;
            }
            // Only test-side files differ: each is judged by what the
            // run's tests did with it, from the run's record of its files.
            let Some(files) = files else {
                outdated.push((
                    *run,
                    "the tests have changed, and its record of its files cannot be read".to_owned(),
                    true,
                ));
                return false;
            };
            let recorded_files = &files.manifest.files;
            let configurations = recorded_files
                .keys()
                .filter(|file| supercov_engine::run_currency::configuration_file(file))
                .cloned()
                .collect::<Vec<_>>();
            let now = supercov_engine::run_currency::checkout_files(
                root,
                recorded_files,
                &configurations,
            );
            let ran = supercov_engine::run_currency::Ran::of(run);
            let playwright_changed = configurations.iter().any(|file| {
                supercov_engine::test_loads::configuration_runner(file) == Some("playwright")
                    && now.get(file).map(|is| &is.sha256)
                        != recorded_files.get(file).map(|was| &was.sha256)
            });
            let reference = if playwright_changed {
                reference()
            } else {
                None
            };
            let mut judgment = supercov_engine::run_currency::judge(
                &checkout,
                &run.metadata.command.join(" "),
                recorded_files,
                &now,
                &|file| fs::read_to_string(root.join(file)).ok(),
                supercov_engine::source_manifest::test_files(run).as_ref(),
                &ran,
                reference.as_ref(),
                allow_changed,
            );
            // A configuration file the run was not recorded with: one
            // was added, and it may set how its tests run.
            if judgment.outdated.is_empty()
                && recorded.fingerprint.configuration != current.fingerprint.configuration
                && !configurations.iter().any(|file| {
                    now.get(file).map(|is| &is.sha256)
                        != recorded_files.get(file).map(|was| &was.sha256)
                })
                && !allow_changed
            {
                judgment
                    .outdated
                    .push("a configuration file was added".to_owned());
            }
            match judgment.outdated.first() {
                Some(_) => {
                    outdated.push((
                        *run,
                        supercov_engine::run_merge::listed(&judgment.outdated),
                        judgment.settled,
                    ));
                    false
                }
                None => true,
            }
        })
        .collect::<Vec<_>>();
    current.sort_by(|left, right| {
        right
            .metadata
            .started_at
            .cmp(&left.metadata.started_at)
            .then_with(|| right.id.cmp(&left.id))
    });
    // One command can make two runs: a browser suite's, and the Python
    // server's its tests drove.
    let measurement = |run: &StoredRun| {
        (
            run.metadata.command.clone(),
            frontend_of(&run.metadata.integrity.instrumenter_version),
        )
    };
    let mut commands = std::collections::BTreeSet::new();
    current.retain(|run| commands.insert((measurement(run), &run.metadata.source_roots)));
    // Not one whose command has been measured again since.
    outdated.retain(|(run, _, _)| {
        !current
            .iter()
            .any(|kept| measurement(kept) == measurement(run))
    });
    outdated.sort_by(|left, right| right.0.metadata.started_at.cmp(&left.0.metadata.started_at));
    let mut said = std::collections::BTreeSet::new();
    outdated.retain(|(run, _, _)| said.insert(measurement(run)));
    for (run, why, settled) in outdated.iter().take(5) {
        println!(
            "[supercov] left out {} ({}): {why}. Measure it again to count it{}.",
            run.id,
            run.metadata.command.join(" "),
            if allow_changed || *settled {
                ""
            } else {
                ", or pass --allow-changed if the change alters nothing its tests ran"
            }
        );
    }
    if current.is_empty() {
        return Err(format!(
            "no run was recorded on the source as it is now, so there is nothing to merge. Measure a suite first: {} -- <test command>",
            launcher_command()
        ));
    }
    // Runs of two languages in one project, or of two packages, are two
    // measurements. Their source, tests and configuration may differ in
    // files they do not measure: what that takes was judged above.
    let mut sets = Vec::<Vec<&StoredRun>>::new();
    current.into_iter().for_each(|run| {
        let measured = |run: &StoredRun| {
            let fingerprint = &run.metadata.integrity.fingerprint;
            (
                fingerprint.dependencies.clone(),
                fingerprint.instrumenter.clone(),
                fingerprint.source_files,
                run.metadata.source_roots.clone(),
            )
        };
        match sets
            .iter_mut()
            .find(|set| measured(set[0]) == measured(run))
        {
            Some(set) => set.push(run),
            None => sets.push(vec![run]),
        }
    });
    // There are runs on the current source, so there is a set of them.
    let mut groups = Vec::new();
    sets.into_iter().for_each(|set| {
        let ids = set.iter().map(|run| run.id.clone()).collect::<Vec<_>>();
        if let [only] = set.as_slice() {
            println!(
                "[supercov] one run on the source as it is now, {} ({}): nothing to merge it with",
                only.id,
                only.metadata.command.join(" ")
            );
            return;
        }
        // The same runs merged before are that merge.
        let wanted = ids.iter().collect::<std::collections::BTreeSet<_>>();
        if let Some(merged) = inventory.runs.iter().find(|run| {
            run.metadata.merged == Some(true)
                && run.metadata.parents.as_ref().is_some_and(|parents| {
                    parents.iter().collect::<std::collections::BTreeSet<_>>() == wanted
                })
        }) {
            println!(
                "[supercov] these {} runs are already merged as {}",
                ids.len(),
                merged.id
            );
            println!("{} runs {}", launcher_command(), merged.id);
            return;
        }
        println!(
            "[supercov] merging {} runs on the source as it is now:",
            ids.len()
        );
        // A set of more than one run.
        set.iter()
            .for_each(|run| println!("  {}  {}", run.id, run.metadata.command.join(" ")));
        groups.push(ids);
    });
    Ok(groups)
}

const PUBLIC_TIMESTAMP_FORMAT: &[time::format_description::BorrowedFormatItem<'static>] =
    format_description!("[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z");

fn public_run_identity() -> (String, String) {
    // A fixed format of the current time: only a year past 9999 fails it.
    let started_at = OffsetDateTime::now_utc()
        .format(PUBLIC_TIMESTAMP_FORMAT)
        .expect("the run timestamp formats any current time");
    let identity = format!(
        "{started_at}\0{}\0{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let digest = format!("{:x}", Sha256::digest(identity.as_bytes()));
    let run_id = format!("run_{}", &digest[..16]);
    (run_id, started_at)
}

/// The warning for a run the checkout no longer matches, naming the first
/// ten files that changed since it.
fn stale_run_warning(run: &str, reasons: &[String], changed: &[String]) -> String {
    let named = if changed.is_empty() {
        String::new()
    } else {
        let shown = changed.iter().take(10).cloned().collect::<Vec<_>>();
        let more = changed.len().saturating_sub(shown.len());
        format!(
            "; changed since the run: {}{}",
            shown.join(", "),
            if more > 0 {
                format!(" and {more} more")
            } else {
                String::new()
            }
        )
    };
    format!("[supercov] stale run {run}: {}{named}", reasons.join(", "))
}

fn public_run_id(value: &str) -> bool {
    value.len() == 20
        && value.starts_with("run_")
        && value[4..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const CHECK_USAGE: &str = "supercov runs <run-id> check [--min-lines <pct>] [--min-statements <pct>]\n                           [--min-functions <pct>] [--min-branches <pct>] [--min-mcdc <pct>]\n                           [--per-file] [--json]\n                           [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>]\n";

/// Gate a recorded run against percentage floors, without running tests again.
///
/// Exit codes follow the CLI contract: 0 passed, 1 a valid measurement that
/// fails policy, 2 a request or evidence that cannot answer the question. The
/// third is the important one -- a failed suite, a stale run, an empty scope or
/// a partly measured metric must never leave a gate green.
fn coverage_check_command(arguments: &[String]) -> ExitCode {
    let mut floors: Vec<supercov_engine::run_view::Floor> = Vec::new();
    let mut per_file = false;
    let mut json = false;
    let mut filter = supercov_engine::coverage_report::TestFilter::default();
    let mut selector: Option<&str> = None;
    let mut index = 0;
    // `runs <id> check ...`; the id may be absent, meaning the latest run.
    let rest = &arguments[1..];
    // Dispatch put the word first or second, so there is a first argument.
    if arguments[0] != "check" {
        selector = Some(arguments[0].as_str());
    }
    while index < rest.len() {
        let argument = rest[index].as_str();
        index += 1;
        if argument == "check" {
            continue;
        }
        match argument {
            "--per-file" => per_file = true,
            "--json" => json = true,
            "--help" | "-h" => {
                print!("{CHECK_USAGE}");
                return ExitCode::SUCCESS;
            }
            flag if flag.starts_with("--min-") => {
                let Some(metric) = supercov_engine::run_view::Metric::parse(&flag[6..]) else {
                    eprintln!("[supercov] unknown metric in {flag}\n{CHECK_USAGE}");
                    return ExitCode::from(2);
                };
                let Some(value) = rest.get(index) else {
                    eprintln!("[supercov] {flag} needs a percentage\n{CHECK_USAGE}");
                    return ExitCode::from(2);
                };
                index += 1;
                match supercov_engine::run_view::parse_percentage(value) {
                    Ok(ppm) => floors.push(supercov_engine::run_view::Floor { metric, ppm }),
                    Err(error) => {
                        eprintln!("[supercov] {flag}: {error}");
                        return ExitCode::from(2);
                    }
                }
            }
            other => {
                if let Some(taken) = test_filter_option(other, rest, &mut index, &mut filter) {
                    if let Err(error) = taken {
                        eprintln!("[supercov] {error}\n{CHECK_USAGE}");
                        return ExitCode::from(2);
                    }
                    continue;
                }
                eprintln!("[supercov] unknown option {other}\n{CHECK_USAGE}");
                return ExitCode::from(2);
            }
        }
    }

    let result = (|| -> Result<supercov_engine::run_view::Outcome, String> {
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        let view =
            load_run_view_filtered(&root, selector, (!filter.is_empty()).then_some(&filter))?;
        let outcome = supercov_engine::run_view::check(&view, &floors, per_file);
        if json {
            print!(
                "{}",
                agent_json::success(
                    "coverage.check",
                    &serde_json::json!({"view": view, "outcome": outcome}),
                    None,
                )
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes))?
            );
        } else {
            print!("{}", render_check(&view, &floors, &outcome));
        }
        Ok(outcome)
    })();
    match result {
        Ok(outcome) => ExitCode::from(outcome.exit_code()),
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn render_percentage(covered: usize, eligible: usize) -> String {
    if eligible == 0 {
        return "n/a".into();
    }
    format!("{:.2}%", covered as f64 * 100.0 / eligible as f64)
}

fn render_floor(ppm: u64) -> String {
    let text = format!("{:.4}", ppm as f64 / 10_000.0);
    let trimmed = text.trim_end_matches('0').trim_end_matches('.');
    format!("{trimmed}%")
}

fn render_check(
    view: &supercov_engine::run_view::RunView,
    floors: &[supercov_engine::run_view::Floor],
    outcome: &supercov_engine::run_view::Outcome,
) -> String {
    use supercov_engine::run_view::Outcome;
    let mut out = format!("Coverage check for {}\n", view.run);
    for floor in floors {
        let metric = view.metric(floor.metric);
        out.push_str(&format!(
            "  {:<11} {:>8}  {}/{}  (floor {})\n",
            metric.metric.name(),
            render_percentage(metric.covered, metric.eligible),
            metric.covered,
            metric.eligible,
            render_floor(floor.ppm),
        ));
    }
    match outcome {
        Outcome::Pass => out.push_str("\nPASS: every floor is met.\n"),
        Outcome::Fail { violations } => {
            out.push_str(&format!("\nFAIL: {} floor(s) not met.\n", violations.len()));
            // A failed check has a floor it did not meet.
            violations.iter().for_each(|violation| {
                let scope = violation.file.as_deref().unwrap_or("(whole run)");
                out.push_str(&format!(
                    "  {scope}: {} {}  {}/{} below {}\n",
                    violation.metric.name(),
                    render_percentage(violation.covered, violation.eligible),
                    violation.covered,
                    violation.eligible,
                    render_floor(violation.floor_ppm),
                ));
                if !violation.uncovered_lines.is_empty() {
                    let shown = violation
                        .uncovered_lines
                        .iter()
                        .take(10)
                        .map(usize::to_string)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let more = violation.uncovered_lines.len().saturating_sub(10);
                    out.push_str(&format!(
                        "    uncovered lines: {shown}{}\n",
                        if more > 0 {
                            format!(", and {more} more")
                        } else {
                            String::new()
                        }
                    ));
                }
            });
        }
        Outcome::Error { reasons } => {
            out.push_str("\nERROR: this run cannot answer the question asked.\n");
            // An error has its reasons.
            reasons
                .iter()
                .for_each(|reason| out.push_str(&format!("  {reason}\n")));
        }
    }
    out.push_str(&uncounted_note(view));
    out
}

const PATCH_USAGE: &str = "supercov runs <run-id> patch --base <ref> [--min-lines <pct>]\n                           [--annotate github] [--max-annotations <n>] [--json]\n                           [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>]\n";

/// The options that pick some of a run's tests, as `check`, `patch` and
/// `report` read them. `Some` when `argument` is one of them: whether its
/// value was there, after taking it.
pub(crate) fn test_filter_option(
    argument: &str,
    rest: &[String],
    index: &mut usize,
    filter: &mut supercov_engine::coverage_report::TestFilter,
) -> Option<Result<(), String>> {
    if !matches!(
        argument,
        "--kind" | "--runner" | "--tag" | "--title" | "--test-file"
    ) {
        return None;
    }
    let Some(value) = rest.get(*index).filter(|value| !value.is_empty()) else {
        return Some(Err(format!("{argument} needs a value")));
    };
    *index += 1;
    match argument {
        "--kind" => filter.kind = Some(value.to_lowercase()),
        "--runner" => filter.runner = Some(value.to_lowercase()),
        "--tag" => filter.tags.push(value.clone()),
        "--title" => filter.titles.push(value.clone()),
        _ => filter.files.push(value.clone()),
    }
    Some(Ok(()))
}

/// Each test `filter` picks in `run`, by name and file, from the run's
/// filtered index: the same tests every query reads under that filter. A
/// filter no test matches is refused, as a query refuses it.
pub(crate) fn picked_tests(
    run: &StoredRun,
    filter: &supercov_engine::coverage_report::TestFilter,
) -> Result<std::collections::BTreeSet<(String, Option<String>)>, String> {
    let container = open_query_index_for(run, Some(filter)).map_err(|error| match &error {
        RunIndexError::Report(ReportError::NoTestMatches(described)) => {
            format!("No tests match {described}")
        }
        _ => format!("Failed to open coverage index: {error}"),
    })?;
    let index = CoverageIndex::new(&container).map_err(|error| error.to_string())?;
    Ok(index
        .test_summaries(supercov_engine::coverage_index::CoverageViewId::All)
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|test| (test.name, test.file))
        .collect())
}

fn git_output(root: &Path, arguments: &[&str]) -> Result<String, String> {
    let output = supercov_engine::fault::io("git", std::io::ErrorKind::NotFound, || {
        Command::new("git")
            .args(arguments)
            .current_dir(root)
            .launch_output()
    })
    .map_err(|error| format!("could not run git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Where `root` is below the top of its repository, with a trailing slash: `web/`.
///
/// Git speaks in repository-relative paths; the run speaks in paths relative to
/// wherever Supercov ran. Translate rather than matching by basename, which
/// would attribute one `index.ts` to another.
fn repository_prefix(root: &Path) -> Result<Option<String>, String> {
    let top = git_output(root, &["rev-parse", "--show-toplevel"])?
        .trim()
        .to_owned();
    let top = fs::canonicalize(&top).unwrap_or_else(|_| PathBuf::from(&top));
    let here = fs::canonicalize(root).unwrap_or_else(|_| root.to_owned());
    Ok(here
        .strip_prefix(&top)
        .ok()
        .map(|rest| rest.to_string_lossy().replace('\\', "/"))
        .filter(|rest| !rest.is_empty())
        .map(|rest| format!("{rest}/")))
}

/// Changed lines between the merge base with `base` and the working tree.
///
/// The merge base is the point the branch diverged, not the previous commit.
/// Comparing against the tip of a target branch that has moved on would report
/// other people's lines as this change's obligation.
fn changed_lines_against(
    root: &Path,
    base: &str,
    measured: &supercov_engine::run_view::RunView,
) -> Result<supercov_engine::patch_view::ChangedLines, String> {
    let merge_base = git_output(root, &["merge-base", base, "HEAD"])
        .map_err(|error| format!(
            "{error}\nCould not find where this branch left {base}. A shallow checkout has no merge base: fetch with enough history (actions/checkout uses fetch-depth: 0), and make sure {base} exists locally."
        ))?
        .trim()
        .to_owned();
    // Names as they are: git quotes one that is not ASCII, and the quoted
    // name matched no file the run measured.
    let diff = git_output(
        root,
        &[
            "-c",
            "core.quotePath=false",
            "diff",
            "--unified=0",
            "--no-color",
            "--find-renames",
            &merge_base,
            "--",
        ],
    )?;
    let prefix = repository_prefix(root)?;
    let relocate = |path: String| -> Option<String> {
        match prefix.as_ref() {
            Some(prefix) => path.strip_prefix(prefix).map(str::to_owned),
            None => Some(path),
        }
    };
    let mut changed = supercov_engine::patch_view::changed_lines(&diff)
        .into_iter()
        .filter_map(|(path, lines)| relocate(path).map(|path| (path, lines)))
        .collect::<supercov_engine::patch_view::ChangedLines>();
    // An untracked file is entirely new, so every line the adapter measured in
    // it is a changed line. Reading the adapter's own set keeps this consistent
    // with the rest of the denominator.
    // `--full-name`: from a directory below the top, git lists these from where
    // it stands, and the diff above from the top.
    for path in git_output(
        root,
        &[
            "ls-files",
            "-z",
            "--others",
            "--exclude-standard",
            "--full-name",
        ],
    )?
    .split('\0')
    {
        let Some(path) = relocate(path.to_owned()) else {
            continue;
        };
        if let Some(file) = measured.file(&path) {
            changed
                .entry(path)
                .or_default()
                .extend(file.measured_lines.iter().copied());
        }
    }
    Ok(changed)
}

/// Report coverage of the lines a change touches.
fn coverage_patch_command(arguments: &[String]) -> ExitCode {
    let mut base: Option<String> = None;
    let mut floor: Option<u64> = None;
    let mut annotate = false;
    let mut max_annotations = 50_usize;
    let mut json = false;
    let mut filter = supercov_engine::coverage_report::TestFilter::default();
    let mut selector: Option<&str> = None;
    // Dispatch put the word first or second, so there is a first argument.
    if arguments[0] != "patch" {
        selector = Some(arguments[0].as_str());
    }
    let rest = &arguments[1..];
    let mut index = 0;
    while index < rest.len() {
        let argument = rest[index].as_str();
        index += 1;
        match argument {
            "patch" => continue,
            "--json" => json = true,
            "--help" | "-h" => {
                print!("{PATCH_USAGE}");
                return ExitCode::SUCCESS;
            }
            "--base" | "--min-lines" | "--annotate" | "--max-annotations" => {
                let Some(value) = rest.get(index) else {
                    eprintln!("[supercov] {argument} needs a value\n{PATCH_USAGE}");
                    return ExitCode::from(2);
                };
                index += 1;
                match argument {
                    "--base" => base = Some(value.clone()),
                    "--min-lines" => match supercov_engine::run_view::parse_percentage(value) {
                        Ok(ppm) => floor = Some(ppm),
                        Err(error) => {
                            eprintln!("[supercov] --min-lines: {error}");
                            return ExitCode::from(2);
                        }
                    },
                    "--annotate" => {
                        if value != "github" {
                            eprintln!("[supercov] --annotate only supports github");
                            return ExitCode::from(2);
                        }
                        annotate = true;
                    }
                    _ => match value.parse::<usize>() {
                        Ok(parsed) => max_annotations = parsed,
                        Err(_) => {
                            eprintln!("[supercov] --max-annotations needs a whole number");
                            return ExitCode::from(2);
                        }
                    },
                }
            }
            other => {
                if let Some(taken) = test_filter_option(other, rest, &mut index, &mut filter) {
                    if let Err(error) = taken {
                        eprintln!("[supercov] {error}\n{PATCH_USAGE}");
                        return ExitCode::from(2);
                    }
                    continue;
                }
                eprintln!("[supercov] unknown option {other}\n{PATCH_USAGE}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(base) = base else {
        eprintln!("[supercov] patch needs --base <ref>\n{PATCH_USAGE}");
        return ExitCode::from(2);
    };

    let result = (|| -> Result<u8, String> {
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        let view =
            load_run_view_filtered(&root, selector, (!filter.is_empty()).then_some(&filter))?;
        // Mapping a patch onto coverage recorded from different source would
        // annotate the wrong lines, which is worse than refusing. A failed
        // suite is different: its evidence still matches the source, so the
        // change's coverage is reported -- as an error, never a pass.
        let blockers = view.blockers();
        if !blockers.is_empty() && !view.stale {
            let reasons = blockers
                .into_iter()
                .map(|blocker| blocker.reason)
                .collect::<Vec<_>>();
            let changed = changed_lines_against(&root, &base, &view)?;
            let patch = supercov_engine::patch_view::build(&view, &changed);
            if json {
                print!(
                    "{}",
                    agent_json::success(
                        "coverage.patch",
                        &serde_json::json!({
                            "result": "error",
                            "reasons": reasons,
                            "base": base,
                            "patch": patch,
                        }),
                        None,
                    )
                    .map_err(|error| format!("response exceeds {} bytes", error.max_bytes))?
                );
            } else {
                eprintln!("[supercov] this run cannot pass a patch gate:");
                // Each blocker is a reason.
                reasons.iter().for_each(|reason| eprintln!("  {reason}"));
                print!("{}", render_patch(&patch, &base, None, false));
            }
            return Ok(2);
        }
        if !blockers.is_empty() {
            let reasons = blockers
                .into_iter()
                .map(|blocker| blocker.reason)
                .collect::<Vec<_>>();
            if json {
                print!(
                    "{}",
                    agent_json::success(
                        "coverage.patch",
                        &serde_json::json!({"result": "error", "reasons": reasons}),
                        None,
                    )
                    .map_err(|error| format!("response exceeds {} bytes", error.max_bytes))?
                );
            } else {
                eprintln!("[supercov] this run cannot map a patch:");
                reasons.iter().for_each(|reason| eprintln!("  {reason}"));
            }
            return Ok(2);
        }
        let changed = changed_lines_against(&root, &base, &view)?;
        let patch = supercov_engine::patch_view::build(&view, &changed);
        let passed = floor.is_none_or(|ppm| patch.is_empty() || patch.meets(ppm));
        if json {
            print!(
                "{}",
                agent_json::success(
                    "coverage.patch",
                    &serde_json::json!({
                        "result": if passed { "pass" } else { "fail" },
                        "base": base,
                        "patch": patch,
                    }),
                    None,
                )
                .map_err(|error| format!("response exceeds {} bytes", error.max_bytes))?
            );
        } else {
            print!("{}", render_patch(&patch, &base, floor, passed));
        }
        if annotate {
            // A workflow reads a file's name from the top of the repository.
            let prefix = repository_prefix(&root)?.unwrap_or_default();
            for annotation in
                supercov_engine::patch_view::annotations(&patch, max_annotations, &prefix)
            {
                println!("{annotation}");
            }
        }
        Ok(u8::from(!passed))
    })();
    match result {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

/// The files no floor covered because discovery could not tell whether they
/// are source and nothing loaded them. Said, never failed on: the suite
/// passes without Supercov, so a gate must not fail for what it cannot tell.
fn uncounted_note(view: &supercov_engine::run_view::RunView) -> String {
    let files = view
        .limitations
        .iter()
        .filter_map(|limitation| serde_json::from_str::<serde_json::Value>(limitation).ok())
        .filter(|limitation| limitation["kind"] == "source-scope")
        .filter_map(|limitation| limitation["file"].as_str().map(str::to_owned))
        .collect::<std::collections::BTreeSet<_>>();
    if files.is_empty() {
        return String::new();
    }
    let shown = files.iter().take(5).cloned().collect::<Vec<_>>().join(", ");
    let more = files.len().saturating_sub(5);
    format!(
        "{} {} outside the source roots that no source imports {} not counted: {shown}{}. \
         If {} your code, name the roots with SUPERCOV_SOURCE_ROOTS; `{} runs {} scope` lists them.\n",
        files.len(),
        if files.len() == 1 { "file" } else { "files" },
        if files.len() == 1 { "is" } else { "are" },
        if more > 0 {
            format!(", and {more} more")
        } else {
            String::new()
        },
        if files.len() == 1 {
            "it is"
        } else {
            "they are"
        },
        launcher_command(),
        view.run
    )
}

fn render_patch(
    patch: &supercov_engine::patch_view::PatchView,
    base: &str,
    floor: Option<u64>,
    passed: bool,
) -> String {
    let mut out = format!("Changed-line coverage against {base}\n");
    if patch.is_empty() {
        out.push_str("  No executable changes.\n");
    } else {
        out.push_str(&format!(
            "  {} of {} changed executable line(s) covered ({})\n",
            patch.covered,
            patch.eligible,
            render_percentage(patch.covered, patch.eligible),
        ));
        for file in patch.files.iter().filter(|f| !f.uncovered.is_empty()) {
            let ranges = supercov_engine::patch_view::ranges(&file.uncovered)
                .into_iter()
                .map(|(start, end)| {
                    if start == end {
                        start.to_string()
                    } else {
                        format!("{start}-{end}")
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!("  {}: {ranges}\n", file.file));
        }
    }
    if !patch.missing_from_run.is_empty() {
        out.push_str("  Changed but not measured by this run:\n");
        patch
            .missing_from_run
            .iter()
            .for_each(|file| out.push_str(&format!("    {file}\n")));
    }
    match (floor, passed) {
        (Some(ppm), false) => out.push_str(&format!("\nFAIL: below {}\n", render_floor(ppm))),
        (Some(ppm), true) if patch.is_empty() => out.push_str(&format!(
            "\nPASS: nothing executable changed, so {} does not apply.\n",
            render_floor(ppm)
        )),
        (Some(ppm), true) => out.push_str(&format!("\nPASS: at or above {}\n", render_floor(ppm))),
        (None, _) => {}
    }
    out
}

/// The files that differ from what a stale run measured, tests and source:
/// those it recorded that are not as they were, and those added since its
/// commit. "Test files changed" alone sent readers comparing commit dates
/// with the run's start to work out which.
fn changed_since_run(root: &Path, run: &StoredRun) -> Vec<String> {
    let Ok(stored) = supercov_engine::source_manifest::load_manifest(run) else {
        return Vec::new();
    };
    let recorded = &stored.manifest.files;
    let mut changed = supercov_engine::source_manifest::changed_files(recorded, |file| {
        fs::read_to_string(root.join(file))
            .ok()
            .map(|text| supercov_engine::source_manifest::FileFingerprint::of(&text).sha256)
    })
    .into_iter()
    .collect::<std::collections::BTreeSet<_>>();
    // Added files are in no record: git names them, of the kinds of file the
    // run measured.
    let extensions = recorded
        .keys()
        .filter_map(|file| Path::new(file).extension())
        .collect::<std::collections::BTreeSet<_>>();
    let mut added = Vec::new();
    if let Some(revision) = run
        .metadata
        .integrity
        .git
        .as_ref()
        .and_then(|git| git.revision.as_deref())
        && let Ok(listed) = git_output(
            root,
            &[
                "diff",
                "-z",
                "--name-only",
                "--relative",
                "--diff-filter=A",
                revision,
            ],
        )
    {
        added.extend(listed.split('\0').map(str::to_owned));
    }
    // NUL-separated, so a name is as it is: not quoted, not trimmed.
    if let Ok(listed) = git_output(root, &["ls-files", "-z", "--others", "--exclude-standard"]) {
        added.extend(listed.split('\0').map(str::to_owned));
    }
    for file in added {
        if !file.is_empty()
            && !recorded.contains_key(&file)
            && Path::new(&file)
                .extension()
                .is_some_and(|extension| extensions.contains(extension))
        {
            changed.insert(file);
        }
    }
    changed.into_iter().collect()
}

/// A run's query index, or, when a filter picks tests, the index of only
/// those tests' coverage, built from the run's evidence on first use.
pub(crate) fn open_query_index_for(
    run: &StoredRun,
    filter: Option<&supercov_engine::coverage_report::TestFilter>,
) -> Result<supercov_engine::query_index::QueryIndex, RunIndexError> {
    match filter {
        Some(filter) => supercov_engine::run_store::open_or_build_filtered_query_index(run, filter),
        None => open_or_rebuild_query_index(run),
    }
}

/// Resolve a run of the project at `root` and build the shared view every
/// workflow command reads, for only the tests `filter` picks.
pub(crate) fn load_run_view_filtered(
    root: &Path,
    selector: Option<&str>,
    filter: Option<&supercov_engine::coverage_report::TestFilter>,
) -> Result<supercov_engine::run_view::RunView, String> {
    if let Some(filter) = filter {
        eprintln!(
            "[supercov] reading only the tests {} picks",
            filter.describe()
        );
    }
    let inventory = public_run_inventory(root).map_err(|error| error.to_string())?;
    let run = select_run(&inventory, selector).map_err(|error| error.to_string())?;
    run_view_of(root, run, filter)
}

/// The view of `run` in the checkout at `root`, for only the tests `filter`
/// picks.
fn run_view_of(
    root: &Path,
    run: &StoredRun,
    filter: Option<&supercov_engine::coverage_report::TestFilter>,
) -> Result<supercov_engine::run_view::RunView, String> {
    let comparison = checkout_comparison(root, run);
    let report = analyze_stored_run_filtered(run, filter)?;
    let passed = run.metadata.test_exit_code == Some(0);
    // A passing run is read whole, as its summary reads it: what passing
    // tests ran alone leaves out what ran outside any test -- a module's top
    // level as it was imported, and all of a run no runner attributes -- so
    // a passing `supercov -- node script.js` read 100% in its summary and 0%
    // in `runs check`. A failed run keeps to what its passing tests ran.
    let view = if passed {
        &report.view
    } else {
        &report.filters.passed
    };
    Ok(supercov_engine::run_view::build(
        &run.id,
        &run.metadata.started_at,
        view,
        passed,
        comparison.stale,
        comparison.reasons,
    ))
}

/// The view of the run `selector` picks in the project at `root` and, for a
/// security assessment,
/// the lines its assessed assertions cover and the tests that ran each
/// covered point: the run read once. Lines are `None` when the run was not
/// assessed, which is not an error and is not the same as nothing asserted:
/// no test was asked.
#[allow(clippy::type_complexity)]
pub(crate) fn load_run_coverage(
    root: &Path,
    selector: Option<&str>,
    security: bool,
) -> Result<
    (
        supercov_engine::run_view::RunView,
        Option<std::collections::BTreeMap<String, std::collections::BTreeSet<u64>>>,
        Vec<(String, usize, Vec<String>)>,
    ),
    String,
> {
    let inventory = public_run_inventory(root).map_err(|error| error.to_string())?;
    let run = select_run(&inventory, selector).map_err(|error| error.to_string())?;
    let view = run_view_of(root, run, None)?;
    if !security {
        return Ok((view, None, Vec::new()));
    }
    Ok((
        view,
        assertions_command::asserted_lines(run),
        point_tests(run)?,
    ))
}

/// Which tests executed which lines in `run`: every covered statement with
/// the tests credited for it. This is the run's own account of what ran
/// together, which no import graph can give: a handler and a helper reached
/// only through a lookup table are still executed by one test.
fn point_tests(run: &StoredRun) -> Result<Vec<(String, usize, Vec<String>)>, String> {
    let coverage = supercov_engine::source_manifest::coverage(run)?;
    Ok(coverage
        .view
        .points
        .iter()
        // A point is covered exactly when some test reached it.
        .filter(|point| !point.tests.is_empty())
        .map(|point| {
            (
                point.meta.file.clone(),
                point.meta.line,
                point
                    .tests
                    .iter()
                    .map(|test| test.as_str().to_owned())
                    .collect(),
            )
        })
        .collect())
}

/// The format names live in the engine module the packaging audit reads, so
/// this file never spells them: Supercov emits those formats and must never
/// look like it invokes the tools they are named after.
fn report_usage() -> String {
    format!(
        "supercov runs <run-id> report --format {} [--output <path>] [--force] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>]\n",
        supercov_engine::coverage_export::FORMATS
    )
}

/// Write the file atomically, so a reader never sees a half-written report and
/// a failed write leaves the previous one intact.
fn write_atomically(path: &Path, contents: &str, force: bool) -> Result<(), String> {
    if path.exists() && !force {
        return Err(format!(
            "{} already exists; pass --force to replace it",
            path.display()
        ));
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    let temporary = path.with_extension(format!(
        "{}.supercov-{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp"),
        std::process::id()
    ));
    fs::write(&temporary, contents)
        .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path).map_err(|error| {
        let _ = fs::remove_file(&temporary);
        format!("could not replace {}: {error}", path.display())
    })
}

/// Export a recorded run in a format other tools already read.
fn coverage_report_command(arguments: &[String]) -> ExitCode {
    let mut format: Option<String> = None;
    let mut output: Option<PathBuf> = None;
    let mut force = false;
    let mut filter = supercov_engine::coverage_report::TestFilter::default();
    let mut selector: Option<&str> = None;
    // Dispatch put the word first or second, so there is a first argument.
    if arguments[0] != "report" {
        selector = Some(arguments[0].as_str());
    }
    let rest = &arguments[1..];
    let mut index = 0;
    while index < rest.len() {
        let argument = rest[index].as_str();
        index += 1;
        match argument {
            "report" => continue,
            "--force" => force = true,
            "--help" | "-h" => {
                print!("{}", report_usage());
                return ExitCode::SUCCESS;
            }
            "--format" | "--output" => {
                let Some(value) = rest.get(index) else {
                    eprintln!("[supercov] {argument} needs a value\n{}", report_usage());
                    return ExitCode::from(2);
                };
                index += 1;
                if argument == "--format" {
                    format = Some(value.to_ascii_lowercase());
                } else {
                    output = Some(PathBuf::from(value));
                }
            }
            // The coverage of some tests alone, from the run as recorded.
            other => {
                if let Some(taken) = test_filter_option(other, rest, &mut index, &mut filter) {
                    if let Err(error) = taken {
                        eprintln!("[supercov] {error}\n{}", report_usage());
                        return ExitCode::from(2);
                    }
                    continue;
                }
                eprintln!("[supercov] unknown option {other}\n{}", report_usage());
                return ExitCode::from(2);
            }
        }
    }
    let Some(format) = format else {
        eprintln!("[supercov] report needs --format\n{}", report_usage());
        return ExitCode::from(2);
    };

    let result = (|| -> Result<(), String> {
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        let view =
            load_run_view_filtered(&root, selector, (!filter.is_empty()).then_some(&filter))?;
        // A report from a failed or stale run is still worth reading, so this
        // writes it and says so on stderr rather than refusing. Only a gate
        // must never go green over one.
        for blocker in view.blockers() {
            eprintln!("[supercov] warning: {}", blocker.reason);
        }
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or(0);
        let contents = if format == "html" {
            // Annotating source that no longer matches the run would point at
            // the wrong lines, so the text is embedded only when the run still
            // describes this checkout. Otherwise the report shows line numbers
            // and says why.
            let (sources, reason) = if view.stale {
                (
                    std::collections::BTreeMap::new(),
                    format!(
                        "The run no longer matches the checkout: {}.",
                        view.stale_reasons.join(", ")
                    ),
                )
            } else {
                let sources = view
                    .files
                    .iter()
                    .filter_map(|file| {
                        let text = fs::read_to_string(root.join(&file.file)).ok()?;
                        Some((file.file.clone(), text))
                    })
                    .collect::<std::collections::BTreeMap<_, _>>();
                (
                    sources,
                    "The file could not be read from this checkout.".to_owned(),
                )
            };
            supercov_engine::coverage_html::html(&view, &sources, &reason)
        } else {
            supercov_engine::coverage_export::export(&view, &format, timestamp)?
        };
        match output {
            Some(path) => {
                // `--output coverage/report` names a directory for the HTML
                // report; the document itself is always one self-contained
                // file, so nothing here ever removes a directory tree.
                let path = if format == "html"
                    && path.extension().is_none_or(|extension| extension != "html")
                {
                    path.join("index.html")
                } else {
                    path
                };
                write_atomically(&path, &contents, force)?;
                // Diagnostics go to stderr so a shell redirect of stdout still
                // captures only the report.
                eprintln!("[supercov] wrote {} ({})", path.display(), format);
            }
            None => print!("{contents}"),
        }
        Ok(())
    })();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

fn public_run_inventory(root: &Path) -> Result<RunInventory, RunStoreError> {
    let mut inventory = discover_runs(root)?;
    // Timestamp-named runs belonged to the pre-release local store contract.
    // They are intentionally not a public compatibility surface: the CLI has
    // one stable identity shape and `supercov runs clean` removes old stores.
    inventory.runs.retain(|run| public_run_id(&run.id));
    Ok(inventory)
}

fn javascript_number(value: f64) -> String {
    let rounded = (value * 10.0).round() / 10.0;
    if rounded.fract() == 0.0 {
        format!("{rounded:.0}")
    } else {
        format!("{rounded:.1}")
    }
}

/// A run this process published, timed: every published run records its
/// timings.
fn print_run_timings(metadata: &supercov_engine::run_store::RunMetadata, command_ms: f64) {
    let timings = metadata
        .timings
        .as_ref()
        .expect("a run this process published records its timings");
    eprintln!(
        "[supercov] timings {}",
        format_run_timings(timings, metadata.duration_ms, Some(command_ms))
    );
}

/// `run_ms` is the engine's own clock, which stops before the run is published;
/// `command_ms`, when the run was published, is the whole command. What lies
/// between is publication -- analysing the evidence once for the per-test
/// execution record and the query index -- and it is reported, so that the total is
/// what the command cost rather than what it cost before publishing.
fn format_run_timings(
    timings: &supercov_engine::run_store::RunTimings,
    run_ms: f64,
    command_ms: Option<f64>,
) -> String {
    let phases = format!(
        "initialization={}ms workspace={}ms setup={}ms build={}ms tests={}ms evidence={}ms",
        javascript_number(timings.initialization_ms),
        javascript_number(timings.workspace_preparation_ms),
        javascript_number(timings.adapter_setup_ms),
        javascript_number(timings.instrumented_build_ms),
        javascript_number(timings.test_command_ms),
        javascript_number(timings.evidence_publication_ms),
    );
    match command_ms {
        Some(command_ms) => format!(
            "{phases} publication={}ms total={}ms",
            javascript_number((command_ms - run_ms).max(0.0)),
            javascript_number(command_ms.max(run_ms)),
        ),
        None => format!("{phases} total={}ms", javascript_number(run_ms)),
    }
}

/// What the caller asked for beyond the command itself.
#[derive(Debug, Clone, Copy, Default)]
struct RunOptions {
    /// Credit every test with what it reached, at whatever the language's
    /// frontend has to do differently to manage it.
    ///
    /// Off by default, because what it costs is not measurement time but the
    /// suite's own: a JUnit project with parallel execution runs in order, and
    /// so does a Go package whose tests call `t.Parallel()`. Where a frontend
    /// attributes exactly at no cost -- one process per test, per-test
    /// coverage maps -- it already does, and this changes nothing.
    exact_attribution: bool,
}

fn process_exit_code(code: i32) -> ExitCode {
    ExitCode::from(u8::try_from(code).unwrap_or(1))
}

/// The status of a run that never measured anything. One that could not
/// start because the project's storage was being cleaned is Supercov's not
/// completing the request, which is 2: 1 reads as the tests failing.
///
/// A command that could not be started exits as a shell's would: 127 when
/// there is no such program, 126 when it may not be run.
fn could_not_measure(error: &str) -> ExitCode {
    if error.contains("storage is being cleaned") {
        ExitCode::from(2)
    } else if let Some(code) = error
        .strip_prefix("the test command exited ")
        .and_then(|rest| rest.split_once(' '))
        .and_then(|(code, rest)| {
            rest.starts_with("before any test reported an outcome")
                .then(|| code.parse::<i32>().ok())
                .flatten()
        })
    {
        // No run, and the command's own status is the answer.
        process_exit_code(code)
    } else if let Some(signal) = supercov_engine::process_supervision::ForwardedSignal::ALL
        .into_iter()
        .find(|signal| error.contains(&format!("interrupted by {};", signal.name())))
    {
        // Interrupted, nothing published: exit as the command did, as a
        // shell reports a signal.
        process_exit_code(signal.exit_code())
    } else if error.contains("could not spawn") && error.contains(": no such program: ") {
        ExitCode::from(127)
    } else if error.contains("could not spawn") && error.contains(": not allowed to run: ") {
        ExitCode::from(126)
    } else {
        ExitCode::from(1)
    }
}

fn public_coverage_run(command: Vec<String>, options: RunOptions) -> ExitCode {
    let command_started = Instant::now();
    let command_ms = || command_started.elapsed().as_secs_f64() * 1000.0;
    if command.is_empty() {
        eprintln!("Usage: supercov -- <test command>");
        return ExitCode::from(2);
    }
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let (run_id, started_at) = public_run_identity();
    // A run that is not recorded must not leave the last one's id for a
    // script to read as its own.
    if let Some(file) = std::env::var_os(RUN_ID_FILE).filter(|file| !file.is_empty())
        && Path::new(&file).is_file()
    {
        let _ = fs::remove_file(&file);
    }
    spawn_trash_sweeper(&root);
    let detection = supercov_engine::frontend_detection::detect_frontends(&root, &command);
    let unsupported =
        supercov_engine::frontend_detection::detect_unsupported_ecosystem(&root, &command);
    // The test command is authoritative: `supercov -- go test` deserves the
    // Go answer even when a package.json would otherwise route the run to the
    // JavaScript frontend, which could only produce an empty, misleading run.
    // A genuinely mixed command that also launches a supported runner still
    // proceeds.
    if let Some(ecosystem) = unsupported.as_ref().filter(|ecosystem| {
        ecosystem.from_command
            && !supercov_engine::frontend_detection::command_launches_supported_frontend(
                &root, &command,
            )
    }) {
        eprintln!(
            "[supercov] This looks like a {} test run ({}), and Supercov does not support {} yet.",
            ecosystem.language, ecosystem.evidence, ecosystem.language
        );
        eprintln!(
            "[supercov] Supercov currently measures JavaScript, TypeScript, Rust, Python, Ruby, Go, Java, and Kotlin test runs. If you'd like {} support, please open an issue or PR: https://github.com/supercorp-ai/supercov",
            ecosystem.language
        );
        return ExitCode::from(2);
    }
    if detection.frontends.is_empty() {
        match unsupported {
            Some(ecosystem) => {
                eprintln!(
                    "[supercov] This looks like a {} project ({}), and Supercov does not support {} yet.",
                    ecosystem.language, ecosystem.evidence, ecosystem.language
                );
                eprintln!(
                    "[supercov] Supercov currently measures JavaScript, TypeScript, Rust, Python, Ruby, Go, Java, and Kotlin test runs. If you'd like {} support, please open an issue or PR: https://github.com/supercorp-ai/supercov",
                    ecosystem.language
                );
            }
            None => {
                eprintln!(
                    "[supercov] Supercov could not recognize this project's language or test framework: no supported test files or manifests were found, and the command does not launch a runner Supercov knows."
                );
                eprintln!(
                    "[supercov] Supercov currently measures JavaScript, TypeScript, Rust, Python, Ruby, Go, Java, and Kotlin test runs. Alternatively, Supercov may simply not support your test suite yet — if so, please open an issue or PR: https://github.com/supercorp-ai/supercov"
                );
            }
        }
        return ExitCode::from(2);
    }
    if detection.frontends.len() > 1 {
        let languages = detection
            .frontends
            .iter()
            .map(|language| format!("{language:?}").to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(", ");
        eprintln!(
            "[supercov] the test command launches multiple language frontends ({languages}); combined polyglot evidence is not implemented yet, so Supercov refuses to publish a partial run"
        );
        return ExitCode::from(2);
    }
    if detection.frontends == [supercov_engine::frontend_detection::FrontendLanguage::Rust] {
        let request = supercov_engine::rust_run::DirectRustRunRequest {
            root: root.clone(),
            command,
            run_id,
            started_at,
        };
        let mut diagnostics = supercov_engine::fault::diagnostics();
        let result = supercov_engine::rust_run::run_direct_rust(&request, &mut diagnostics);
        finish_trash_sweep(&root);
        return match result {
            Ok(result) => {
                published(&result.run_directory);
                eprintln!(
                    "[supercov] Rust coverage: {} test(s) across {} artifact(s)",
                    result.tests, result.artifacts
                );
                print_run_timings(&result.metadata, command_ms());
                process_exit_code(result.exit_code)
            }
            Err(error) => {
                eprintln!("[supercov] {error}");
                could_not_measure(&error)
            }
        };
    }
    if detection.frontends == [supercov_engine::frontend_detection::FrontendLanguage::Go] {
        let request = supercov_engine::go_run::DirectGoRunRequest {
            root: root.clone(),
            command,
            run_id,
            started_at,
            exact_attribution: options.exact_attribution,
        };
        let mut diagnostics = supercov_engine::fault::diagnostics();
        let result = supercov_engine::go_run::run_direct_go(&request, &mut diagnostics);
        finish_trash_sweep(&root);
        return match result {
            Ok(result) => {
                published(&result.run_directory);
                eprintln!(
                    "[supercov] Go coverage: {} test(s) across {} source file(s) in {} package(s)",
                    result.tests, result.source_files, result.packages
                );
                // A run of nothing but parallel tests measures a whole package
                // and can name none of it. Printing "0 test(s)" and stopping
                // reads as an empty suite, which is a different thing and the
                // wrong thing to act on.
                if result.unattributed > 0 {
                    eprintln!(
                        "[supercov] {} test(s) could not announce themselves (t.Parallel, Example or Fuzz); what they reached counts run-wide and belongs to no test",
                        result.unattributed
                    );
                }
                // A hole in the denominator travels with the number it was
                // taken out of: the warning above scrolls past, this is on the
                // line that reports the result.
                if result.unparseable_sources > 0 {
                    eprintln!(
                        "[supercov] {} source file(s) did not parse and are absent from the denominator; the run records them as a blocking limitation",
                        result.unparseable_sources
                    );
                }
                // The tests in it still ran, and what they reached still
                // counts; what is missing is the name to put it under.
                if result.unparseable_tests > 0 {
                    eprintln!(
                        "[supercov] {} test file(s) did not parse; the tests they declare ran unattributed",
                        result.unparseable_tests
                    );
                }
                print_run_timings(&result.metadata, command_ms());
                process_exit_code(result.exit_code)
            }
            Err(error) => {
                eprintln!("[supercov] {error}");
                could_not_measure(&error)
            }
        };
    }
    if detection.frontends == [supercov_engine::frontend_detection::FrontendLanguage::Jvm] {
        let request = supercov_engine::jvm_run::DirectJvmRunRequest {
            root: root.clone(),
            command,
            run_id,
            started_at,
            exact_attribution: options.exact_attribution,
        };
        let mut diagnostics = supercov_engine::fault::diagnostics();
        let result = supercov_engine::jvm_run::run_direct_jvm(&request, &mut diagnostics);
        finish_trash_sweep(&root);
        return match result {
            Ok(result) => {
                published(&result.run_directory);
                eprintln!(
                    "[supercov] JVM coverage: {} test(s) across {} source file(s) in {} module(s)",
                    result.tests, result.source_files, result.modules
                );
                print_run_timings(&result.metadata, command_ms());
                process_exit_code(result.exit_code)
            }
            Err(error) => {
                eprintln!("[supercov] {error}");
                could_not_measure(&error)
            }
        };
    }
    if detection.frontends == [supercov_engine::frontend_detection::FrontendLanguage::Python] {
        let request = supercov_engine::python_run::DirectPythonRunRequest {
            root: root.clone(),
            command,
            run_id,
            started_at,
        };
        let mut diagnostics = supercov_engine::fault::diagnostics();
        let result = supercov_engine::python_run::run_direct_python(&request, &mut diagnostics);
        finish_trash_sweep(&root);
        return match result {
            Ok(result) => {
                published(&result.run_directory);
                eprintln!(
                    "[supercov] Python coverage: {} test(s) across {} source file(s), {} interpreter process(es) on Python {}",
                    result.tests,
                    result.source_files,
                    result.interpreters,
                    result.python_versions.join(", ")
                );
                print_run_timings(&result.metadata, command_ms());
                process_exit_code(result.exit_code)
            }
            Err(error) => {
                eprintln!("[supercov] {error}");
                could_not_measure(&error)
            }
        };
    }
    if detection.frontends == [supercov_engine::frontend_detection::FrontendLanguage::Ruby] {
        let request = supercov_engine::ruby_run::DirectRubyRunRequest {
            root: root.clone(),
            command,
            run_id,
            started_at,
        };
        let mut diagnostics = supercov_engine::fault::diagnostics();
        let result = supercov_engine::ruby_run::run_direct_ruby(&request, &mut diagnostics);
        finish_trash_sweep(&root);
        return match result {
            Ok(result) => {
                published(&result.run_directory);
                eprintln!(
                    "[supercov] Ruby coverage: {} test(s) across {} source file(s), {} interpreter process(es) on Ruby {}",
                    result.tests,
                    result.source_files,
                    result.interpreters,
                    result.ruby_versions.join(", ")
                );
                print_run_timings(&result.metadata, command_ms());
                process_exit_code(result.exit_code)
            }
            Err(error) => {
                eprintln!("[supercov] {error}");
                could_not_measure(&error)
            }
        };
    }
    let watchdog_program = std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok());
    let command_text = command.join(" ");
    let request = supercov_engine::javascript_run::DirectJavascriptRunRequest {
        root: root.clone(),
        command,
        run_id: Some(run_id),
        started_at: Some(started_at),
        watchdog_program,
    };
    let mut diagnostics = supercov_engine::fault::diagnostics();
    let result = supercov_engine::javascript_run::run_direct_javascript(&request, &mut diagnostics);
    finish_trash_sweep(&root);
    match result {
        Ok(result) => {
            published(&result.run_directory);
            if result.exit_code != 0 {
                explain_failed_command(
                    &root,
                    &result.run_id,
                    result.exit_code,
                    &result.unrun_build,
                    &command_text,
                );
            }
            // A server's run directory is named by its run id.
            for (language, server) in &result.server_runs {
                let id = server.file_name().unwrap_or_default().to_string_lossy();
                eprintln!(
                    "[supercov] the {language} server's coverage is run {id}: `supercov runs {id}`"
                );
            }
            print_run_timings(&result.metadata, command_ms());
            process_exit_code(result.exit_code)
        }
        Err(supercov_engine::javascript_run::DirectJavascriptRunError::Interrupted {
            exit_code,
            timings,
            total_ms,
            ..
        }) => {
            eprintln!(
                "[supercov] timings {}",
                format_run_timings(&timings, total_ms, None)
            );
            process_exit_code(exit_code)
        }
        Err(error) => {
            eprintln!("[supercov] {error}");
            could_not_measure(&error.to_string())
        }
    }
}

fn parse_cleanup_options(arguments: &[String]) -> Result<(usize, bool), String> {
    let mut keep = 0;
    let mut dry_run = false;
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        if argument == "--dry-run" {
            dry_run = true;
        } else if argument == "--keep" {
            index += 1;
            let Some(value) = arguments.get(index) else {
                return Err("--keep must be a non-negative integer".into());
            };
            keep = value
                .parse::<usize>()
                .map_err(|_| "--keep must be a non-negative integer".to_owned())?;
        } else {
            return Err(format!("Unknown clean option: {argument}"));
        }
        index += 1;
    }
    Ok((keep, dry_run))
}

fn cleanup_summary(
    keep: usize,
    dry_run: bool,
    result: &supercov_engine::lifecycle::CleanupResult,
) -> String {
    format!(
        "[supercov] {} {} stored run(s), {} per-run workspace(s), {}and {} isolated build cache; keeping {} newest run(s)",
        if dry_run { "would remove" } else { "removed" },
        result.removed_runs.len(),
        result.removed_workspaces.len(),
        // Named separately because it is not a run: it is what a run Supercov
        // failed to publish measured, and removing it is irreversible.
        if result.removed_failed_evidence.is_empty() {
            String::new()
        } else {
            format!(
                "the kept evidence of {} failed run(s), ",
                result.removed_failed_evidence.len()
            )
        },
        if result.removed_build_cache {
            "the"
        } else {
            "no"
        },
        keep,
    )
}

fn cleanup_command(arguments: Vec<String>) -> ExitCode {
    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print!(
            "Usage: supercov runs clean [--keep N] [--dry-run]\n\nRemoves all stored runs and Supercov's isolated build cache by default.\nUse --keep N to retain the N newest runs.\n\nSaved quality assessments are never removed here; they cost money to\nproduce and are not reproducible. Use supercov quality clean for those.\nGenerated reports in .supercov/reports are left alone too.\n"
        );
        return ExitCode::SUCCESS;
    }
    let (keep, dry_run) = match parse_cleanup_options(&arguments) {
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
    spawn_trash_sweeper(&root);
    let options = supercov_engine::lifecycle::CleanupOptions { keep, dry_run };
    let updated_at = format!(
        "unix-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    );
    let result = supercov_engine::lifecycle::clean_storage(&root, options, &updated_at);
    let result = match result {
        Ok(result) => result,
        Err(error) => {
            eprintln!("[supercov] clean failed: {error}");
            return ExitCode::from(2);
        }
    };
    spawn_trash_sweeper(&root);
    println!("{}", cleanup_summary(keep, dry_run, &result));
    for id in result.removed_runs {
        println!("{id}");
    }
    ExitCode::SUCCESS
}

fn internal_agent_error(message: impl Into<String>) -> agent_json::AgentError {
    agent_json::AgentError {
        code: agent_json::ErrorCode::InternalError,
        message: message.into(),
        retryable: false,
        details: None,
    }
}

fn run_store_agent_error(error: RunStoreError) -> agent_json::AgentError {
    match error {
        RunStoreError::NoRuns => agent_json::AgentError {
            code: agent_json::ErrorCode::NoRuns,
            message: "No local coverage runs. Run supercov first.".into(),
            retryable: false,
            details: None,
        },
        RunStoreError::RunNotFound(selector) => agent_json::AgentError {
            code: agent_json::ErrorCode::RunNotFound,
            message: format!("Coverage run not found: {selector}"),
            retryable: false,
            details: Some(serde_json::json!({ "selector": selector })),
        },
        error => internal_agent_error(format!("Failed to read local coverage runs: {error}")),
    }
}

fn current_javascript_integrity(
    root: &Path,
    command: &[String],
    source_roots: Option<&[String]>,
) -> Option<supercov_engine::run_store::RunIntegrity> {
    supercov_engine::javascript_run::current_javascript_integrity(root, command, source_roots).ok()
}

/// Which frontend measured a run, from the contract version it recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Frontend {
    Rust,
    Ruby,
    Python,
    Go,
    Jvm,
    JavaScript,
}

/// Only the frontend that measured a run can say what its checkout looks like
/// now. JavaScript is the last resort rather than the default for anything
/// unrecognised -- a mapping that silently answered for Go and the JVM, which
/// were added later and never listed. samber/lo is Go with a JavaScript docs
/// site: every one of its runs compared a Go fingerprint against a JavaScript
/// one and reported five simultaneous changes to a checkout nobody had
/// touched. A Go or JVM project with no package.json fared no better -- the
/// JavaScript answer was None and staleness silently stopped being checked at
/// all.
fn frontend_of(instrumenter_version: &str) -> Frontend {
    for (prefix, frontend) in [
        ("supercov-rust-", Frontend::Rust),
        ("supercov-ruby-", Frontend::Ruby),
        ("supercov-python-", Frontend::Python),
        ("supercov-go-", Frontend::Go),
        ("supercov-jvm-", Frontend::Jvm),
    ] {
        if instrumenter_version.starts_with(prefix) {
            return frontend;
        }
    }
    Frontend::JavaScript
}

/// Says, of each test that asserted and recorded no coverage, whether its
/// file imports any measured source. One that imports none can only have
/// checked test code, mocks or fixtures, which Supercov does not measure, and
/// is told apart from one whose evidence may have been lost. Left unsaid for
/// a browser runner's tests, which reach the app over HTTP, and where the
/// imports cannot be read.
fn mark_tests_that_import_no_measured_source(
    root: &Path,
    run: &StoredRun,
    tests: &mut [supercov_engine::coverage_query::TestWithoutEvidence],
) {
    let Some(measured) = supercov_engine::run_currency::Ran::of(run).measured else {
        return;
    };
    let imports = supercov_engine::test_loads::Imports::new(root);
    for test in tests {
        let Some(file) = test.file.as_deref() else {
            continue;
        };
        if test.runner == "playwright"
            || test.kind == "e2e"
            || !supercov_engine::test_loads::reads_imports(file)
        {
            continue;
        }
        let loads = imports.loads([file]);
        if loads.open.is_none() {
            test.imports_measured = Some(loads.files.iter().any(|file| measured.contains(file)));
        }
    }
}

/// A run against the checkout as it is now (`compare_with_checkout`).
pub(crate) fn checkout_comparison(
    root: &Path,
    run: &StoredRun,
) -> supercov_engine::run_store::IntegrityComparison {
    supercov_engine::run_store::compare_with_checkout(
        &run.metadata.integrity,
        current_integrity_for_run(root, run).as_ref(),
    )
}

fn current_integrity_for_run(
    root: &Path,
    run: &StoredRun,
) -> Option<supercov_engine::run_store::RunIntegrity> {
    let (command, roots) = integrity_command(root, run);
    let (command, roots) = (&command, roots.as_deref());
    match frontend_of(&run.metadata.integrity.instrumenter_version) {
        Frontend::Rust => {
            supercov_engine::rust_run::current_rust_integrity(root, command, roots).ok()
        }
        Frontend::Ruby => {
            supercov_engine::ruby_run::current_ruby_integrity(root, command, roots).ok()
        }
        Frontend::Python => {
            supercov_engine::python_run::current_python_integrity(root, command, roots).ok()
        }
        Frontend::Go => supercov_engine::go_run::current_go_integrity(root, command, roots).ok(),
        Frontend::Jvm => supercov_engine::jvm_run::current_jvm_integrity(root, command, roots).ok(),
        Frontend::JavaScript => current_javascript_integrity(root, command, roots),
    }
}

/// The command a run's fingerprint was made with. A merged run carries the
/// fingerprint of the run it was merged into, and where a language's
/// fingerprint holds the test command, checking it against `supercov merge`
/// made every merged run of it read as out of date.
fn integrity_command(root: &Path, run: &StoredRun) -> (Vec<String>, Option<Vec<String>>) {
    let own = || {
        (
            run.metadata.command.clone(),
            run.metadata.source_roots.clone(),
        )
    };
    if run.metadata.merged != Some(true) {
        return own();
    }
    let Ok(inventory) = public_run_inventory(root) else {
        return own();
    };
    let combined = &run.metadata.integrity.fingerprint.combined;
    let mut current = run;
    // A merge of merges: follow the fingerprint down to a run of a command.
    for _ in 0..16 {
        let Some(parent) = current
            .metadata
            .parents
            .iter()
            .flatten()
            .filter_map(|id| inventory.runs.iter().find(|stored| stored.id == *id))
            .find(|parent| parent.metadata.integrity.fingerprint.combined == *combined)
        else {
            break;
        };
        if parent.metadata.merged != Some(true) {
            return (
                parent.metadata.command.clone(),
                parent.metadata.source_roots.clone(),
            );
        }
        current = parent;
    }
    own()
}

fn selected_package_script(command: &[String]) -> Option<&str> {
    let program = command
        .first()
        .and_then(|value| Path::new(value).file_stem())
        .and_then(OsStr::to_str)?;
    if !matches!(program, "npm" | "pnpm" | "yarn") {
        return None;
    }
    // Flags go before or after the subcommand (`npm -s run test:unit`,
    // `npm run --silent test:unit`), and npm takes `t` and `run-script` for
    // the commands they abbreviate.
    let mut words = command[1..]
        .iter()
        .map(String::as_str)
        .filter(|word| !word.starts_with('-'));
    match words.next() {
        Some("test" | "t" | "tst") => Some("test"),
        Some("run" | "run-script") => words.next(),
        Some(value) if program != "npm" => Some(value),
        _ => None,
    }
}

fn full_suite_hints(root: &Path, command: &[String], observed_kinds: &[String]) -> Vec<String> {
    if observed_kinds.len() != 1 {
        return Vec::new();
    }
    let Ok(raw) = fs::read_to_string(root.join("package.json")) else {
        return Vec::new();
    };
    let Ok(package) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    let Some(scripts) = package
        .get("scripts")
        .and_then(serde_json::Value::as_object)
    else {
        return Vec::new();
    };
    if !scripts.contains_key("test") {
        return Vec::new();
    }
    let selected = selected_package_script(command);
    if selected == Some("test") {
        return Vec::new();
    }
    let alternatives = scripts
        .keys()
        .filter(|name| name.starts_with("test:") && Some(name.as_str()) != selected)
        .take(4)
        .cloned()
        .collect::<Vec<_>>();
    if selected.is_none() && alternatives.is_empty() {
        return Vec::new();
    }
    let suites = if alternatives.is_empty() {
        String::new()
    } else {
        format!(" Other test scripts include {}.", alternatives.join(", "))
    };
    vec![format!(
        "This run observed only the `{}` test kind and wrapped `{}`.{suites} For whole-project coverage, measure the package's full test command: `{} -- npm test`.",
        observed_kinds[0],
        command.join(" "),
        launcher_command()
    )]
}

/// Record in a file's data, which a reader takes for the whole file, that
/// this page was cut to fit one reply. The record is part of the reply, so
/// the page is fitted with it.
fn record_page_cut(output: &mut IndexedQueryOutput, asked: usize) {
    let page = output.pagination.clone().expect(PAGED);
    if let supercov_engine::indexed_query::IndexedQueryData::FileDetail(data) = &mut output.data {
        data.page_cut = Some(supercov_engine::coverage_query::PageCut {
            asked,
            returned: page.returned,
            next_offset: page.next_offset,
        });
    }
}

/// Say on stderr that this page was cut to fit one reply.
fn say_page_cut(output: &IndexedQueryOutput, asked: usize) {
    let page = output.pagination.as_ref().expect(PAGED);
    // A page cut to fit always has more after it: what fits is fewer than
    // were asked for, and as many were there to return.
    eprintln!(
        "[supercov] this page holds {} of the {asked} entries asked for, as many as fit in one reply ({} in all); the rest start at --offset {}",
        page.returned,
        page.total,
        page.offset + page.returned
    );
}

/// How many entries of a page too large for one reply would fit, when it
/// is that and not something else.
fn page_that_fits(output: &IndexedQueryOutput) -> Option<usize> {
    match output.agent_json() {
        Err(supercov_engine::indexed_query::IndexedQueryError::PageTooLarge { fits, .. }) => {
            Some(fits)
        }
        _ => None,
    }
}

/// A page is cut to fit only where it is paginated.
const PAGED: &str = "a page cut to fit is paginated";

enum PublicQueryOutput {
    Runs {
        data: RunListData,
        pagination: supercov_contracts::AgentPagination,
        /// The outcome filter the listing was asked for, for its next page.
        filter: String,
    },
    Coverage {
        output: IndexedQueryOutput,
        warnings: Vec<String>,
        /// The request as it was asked, which its rendering names again.
        request: Box<IndexedQueryRequest>,
    },
}

impl PublicQueryOutput {
    /// Several lines too many for one reply keep the ones that fit and name
    /// the rest, to be asked again.
    fn fit_lines(&mut self) {
        let Self::Coverage { output, .. } = self else {
            return;
        };
        loop {
            if output.agent_json().is_ok() {
                return;
            }
            let supercov_engine::indexed_query::IndexedQueryData::Lines(data) = &mut output.data
            else {
                return;
            };
            if data.lines.len() <= 1 {
                return;
            }
            let line = data.lines.pop().expect("more than one line");
            data.pages.pop();
            let location = match &line {
                supercov_engine::coverage_query::CoverageCoversData::Anchors(data) => {
                    &data.location
                }
                supercov_engine::coverage_query::CoverageCoversData::Line(data) => &data.location,
            };
            data.remaining
                .insert(0, format!("{}:{}", location.file, location.line));
            // The standard page: a pager following `nextOffset` asks for the
            // rest as it asks for any page.
            let returned = data.lines.len();
            let page = output
                .pagination
                .as_mut()
                .expect("a page of lines is paginated");
            page.returned = returned;
            page.limit = returned;
            page.has_more = true;
            page.next_offset = Some(page.offset + returned);
        }
    }

    fn agent_json(&self) -> Result<String, agent_json::AgentError> {
        match self {
            Self::Runs {
                data, pagination, ..
            } => agent_json::success("runs", data, Some(pagination)).map_err(|error| {
                supercov_engine::indexed_query::IndexedQueryError::ResponseTooLarge(error)
                    .agent_error()
            }),
            Self::Coverage { output, .. } => {
                output.agent_json().map_err(|error| error.agent_error())
            }
        }
    }
}

struct PublicQueryExecutionError {
    error: Box<agent_json::AgentError>,
    warnings: Vec<String>,
}

impl From<agent_json::AgentError> for PublicQueryExecutionError {
    fn from(error: agent_json::AgentError) -> Self {
        Self {
            error: Box::new(error),
            warnings: Vec::new(),
        }
    }
}

/// What the assertion map says about a line, beside its coverage.
fn line_assertions(
    run: &StoredRun,
    data: &mut supercov_engine::coverage_query::CoverageCoversData,
) {
    use supercov_engine::coverage_query::CoverageCoversData;
    match data {
        CoverageCoversData::Anchors(data) => {
            data.assertions = Some(assertions_command::for_line(
                run,
                &data.location.file,
                data.location.line,
            ));
        }
        CoverageCoversData::Line(data) => {
            data.assertions = Some(assertions_command::for_line(
                run,
                &data.location.file,
                data.location.line,
            ));
        }
    }
}

fn execute_public_query(
    root: &Path,
    invocation: &PublicQueryInvocation,
) -> Result<PublicQueryOutput, PublicQueryExecutionError> {
    match invocation {
        PublicQueryInvocation::Runs {
            filter,
            offset,
            limit,
            ..
        } => {
            let inventory = public_run_inventory(root).map_err(run_store_agent_error)?;
            use supercov_engine::coverage_index::CoverageViewId;
            let view = [
                CoverageViewId::All,
                CoverageViewId::Passed,
                CoverageViewId::Failed,
            ]
            .into_iter()
            .find(|view| view.name() == filter)
            .expect("the public parser validates coverage filters");
            let (data, page) = run_list_query(
                &inventory,
                &|run| current_integrity_for_run(root, run),
                view,
                *offset,
                *limit,
            );
            Ok(PublicQueryOutput::Runs {
                data,
                pagination: page,
                filter: filter.clone(),
            })
        }
        PublicQueryInvocation::Coverage {
            request,
            newer_run_id,
            ..
        } => execute_coverage_query(root, request, newer_run_id.as_deref()).map(
            |(output, warnings)| PublicQueryOutput::Coverage {
                output,
                warnings,
                request: request.clone(),
            },
        ),
    }
}

/// A coverage query, answered from its run's index, with the warnings the
/// run's state calls for.
fn execute_coverage_query(
    root: &Path,
    request: &IndexedQueryRequest,
    newer_run_id: Option<&str>,
) -> Result<(IndexedQueryOutput, Vec<String>), PublicQueryExecutionError> {
    let mut request = request.clone();
    let inventory = public_run_inventory(root).map_err(run_store_agent_error)?;
    let run = select_run(&inventory, Some(&request.run_id)).map_err(run_store_agent_error)?;
    request.run_id.clone_from(&run.id);
    request
        .valid
        .get_or_insert(run.metadata.test_exit_code == Some(0));
    request.test_exit_code = run.metadata.test_exit_code;
    let mut warnings = Vec::new();
    let mut diff_inputs = None;
    {
        let comparison = checkout_comparison(root, run);
        request.stale.get_or_insert(comparison.stale);
        request
            .stale_reasons
            .get_or_insert_with(|| comparison.reasons.clone());
        // A query about some files is stale when those files
        // changed, or what decides every file did; tests added
        // since the run change nothing a line of source reads.
        let queried = queried_files(&request);
        let changed = if comparison.stale {
            changed_since_run(root, run)
        } else {
            Vec::new()
        };
        let scoped_stale = |changed: &[String]| {
            comparison.reasons.iter().any(|reason| {
                reason != "test files changed" && reason != "instrumented source changed"
            }) || queried.iter().any(|file| changed.contains(file))
        };
        if comparison.stale && !queried.is_empty() {
            if scoped_stale(&changed) {
                let mine = queried
                    .iter()
                    .filter(|file| changed.contains(file))
                    .cloned()
                    .collect::<Vec<_>>();
                warnings.push(if mine.is_empty() {
                    format!(
                        "[supercov] stale run {}: {}",
                        run.id,
                        comparison.reasons.join(", ")
                    )
                } else {
                    format!(
                        "[supercov] stale run {}: {} changed since the run",
                        run.id,
                        mine.join(", ")
                    )
                });
            }
        } else if comparison.stale {
            warnings.push(stale_run_warning(&run.id, &comparison.reasons, &changed));
        }
    }
    let result = (|| -> Result<IndexedQueryOutput, agent_json::AgentError> {
        // One line past the end of the file is no line: reading it as
        // code the run did not measure would be an answer.
        // A `line` request always names its file and line.
        let (file, line) = (
            request.file.as_deref().unwrap_or_default(),
            request.line.unwrap_or_default(),
        );
        if request.command == "line"
            && request.more_lines.is_empty()
            && let Ok(contents) = fs::read_to_string(root.join(file))
            && line > contents.lines().count()
        {
            return Err(agent_json::AgentError {
                code: agent_json::ErrorCode::InvalidArgument,
                message: format!(
                    "{file} has {} lines; there is no line {line}",
                    contents.lines().count()
                ),
                retryable: false,
                details: Some(serde_json::json!({ "file": file, "line": line })),
            });
        }
        let progress = ProgressLine::start("loading run evidence");
        let test_filter = request.test_filter();
        let container = open_query_index_for(run, test_filter.as_ref()).map_err(|error| {
            if let RunIndexError::Report(ReportError::NoTestMatches(filter)) = &error {
                return agent_json::AgentError {
                    code: agent_json::ErrorCode::TestFilterEmpty,
                    message: format!("No tests match {filter}"),
                    retryable: false,
                    details: Some(serde_json::json!({ "filter": filter })),
                };
            }
            let message = if matches!(error, RunIndexError::Report(ReportError::NoEvidence(_))) {
                no_evidence_message(run.metadata.test_exit_code)
            } else {
                format!("Failed to open coverage index: {error}")
            };
            internal_agent_error(message)
        })?;
        let index = CoverageIndex::new(&container).map_err(|error| {
            internal_agent_error(format!("Failed to read coverage index: {error}"))
        })?;
        drop(progress);
        let report = if request.command == "minimize" {
            Some(
                analyze_stored_run_filtered(run, test_filter.as_ref())
                    .map_err(|error| internal_agent_error(error.to_string()))?,
            )
        } else {
            None
        };

        let newer_container;
        let newer_index;
        let newer_query = if request.command == "diff" {
            let selector = newer_run_id.expect("a parsed diff names its newer run");
            let newer_run =
                select_run(&inventory, Some(selector)).map_err(run_store_agent_error)?;
            // A run compared with itself was warned about above.
            if newer_run.id != run.id {
                let comparison = checkout_comparison(root, newer_run);
                if comparison.stale {
                    warnings.push(stale_run_warning(
                        &newer_run.id,
                        &comparison.reasons,
                        &changed_since_run(root, newer_run),
                    ));
                }
            }
            let (older, newer) = (
                &run.metadata.integrity.fingerprint,
                &newer_run.metadata.integrity.fingerprint,
            );
            // A stored run's fingerprints are sha256 digests, or it
            // is not read at all.
            diff_inputs = Some(supercov_engine::coverage_query::CoverageDiffInputs {
                source_changed: older.source != newer.source
                    || older.dependencies != newer.dependencies
                    || older.configuration != newer.configuration,
                tests_changed: older.tests != newer.tests,
            });
            let comparison_progress = ProgressLine::start("loading comparison run");
            newer_container =
                open_query_index_for(newer_run, test_filter.as_ref()).map_err(|error| {
                    internal_agent_error(format!("Failed to open newer coverage index: {error}"))
                })?;
            newer_index = CoverageIndex::new(&newer_container).map_err(|error| {
                internal_agent_error(format!("Failed to read newer coverage index: {error}"))
            })?;
            drop(comparison_progress);
            Some(NewerQuery {
                run_id: &newer_run.id,
                index: &newer_index,
            })
        } else {
            None
        };
        query_indexed(&index, report.as_ref(), &request, newer_query)
            .map_err(|error| error.agent_error())
    })();
    // A line past the end of its file is no line, as it is when asked
    // alone: it fails, the others stand; all failing fails the call.
    let result = result.and_then(|mut output| {
        if let supercov_engine::indexed_query::IndexedQueryData::Lines(data) = &mut output.data {
            let mut kept = Vec::new();
            let mut pages = Vec::new();
            for (line, page) in std::mem::take(&mut data.lines)
                .into_iter()
                .zip(std::mem::take(&mut data.pages))
            {
                let location = match &line {
                    supercov_engine::coverage_query::CoverageCoversData::Line(data) => {
                        &data.location
                    }
                    supercov_engine::coverage_query::CoverageCoversData::Anchors(data) => {
                        &data.location
                    }
                };
                let lines_in_file = fs::read_to_string(root.join(&location.file))
                    .ok()
                    .map(|contents| contents.lines().count());
                match lines_in_file {
                    Some(count) if location.line > count => {
                        data.failed
                            .push(supercov_engine::indexed_query::CoverageLineFailure {
                                location: format!("{}:{}", location.file, location.line),
                                code: agent_json::ErrorCode::InvalidArgument,
                                message: format!(
                                    "{} has {count} lines; there is no line {}",
                                    location.file, location.line
                                ),
                            });
                    }
                    _ => {
                        kept.push(line);
                        pages.push(page);
                    }
                }
            }
            data.lines = kept;
            data.pages = pages;
            if data.lines.is_empty()
                && let Some(first) = data.failed.first()
            {
                return Err(agent_json::AgentError {
                    code: first.code,
                    message: first.message.clone(),
                    retryable: false,
                    details: Some(serde_json::json!({ "location": first.location })),
                });
            }
        }
        Ok(output)
    });
    result
            .map(|mut output| {
                let recorded = matches!(
                    output.data,
                    supercov_engine::indexed_query::IndexedQueryData::Line(_)
                        | supercov_engine::indexed_query::IndexedQueryData::Lines(_)
                )
                .then(|| supercov_engine::source_manifest::load_manifest(run).ok())
                .flatten()
                .map(|stored| stored.manifest.files);
                if let supercov_engine::indexed_query::IndexedQueryData::Line(data) =
                    &mut output.data
                {
                    hydrate_line_source_from_working_tree(root, recorded.as_ref(), data);
                }
                // Each line of several is given what one line is given.
                if let supercov_engine::indexed_query::IndexedQueryData::Lines(data) =
                    &mut output.data
                {
                    for line in &mut data.lines {
                        hydrate_line_source_from_working_tree(root, recorded.as_ref(), line);
                        line_assertions(run, line);
                    }
                }
                if let supercov_engine::indexed_query::IndexedQueryData::Diff(data) =
                    &mut output.data
                {
                    data.inputs = diff_inputs;
                }
                match &mut output.data {
                    supercov_engine::indexed_query::IndexedQueryData::FileDetail(data) => {
                        data.assertions = Some(assertions_command::for_file(run, &data.file));
                    }
                    supercov_engine::indexed_query::IndexedQueryData::Line(data) => {
                        line_assertions(run, data);
                    }
                    _ => {}
                }
                if let supercov_engine::indexed_query::IndexedQueryData::TestsWithoutEvidence(
                    data,
                ) = &mut output.data
                {
                    mark_tests_that_import_no_measured_source(root, run, &mut data.tests);
                }
                if let supercov_engine::indexed_query::IndexedQueryData::Summary(data) =
                    &mut output.data
                {
                    data.command.clone_from(&run.metadata.command);
                    data.merged_across = run.metadata.merged_across.clone().unwrap_or_default();
                    data.companions = run.metadata.companions.clone().unwrap_or_default();
                    data.assertion_coverage = Some(match assertions_command::saved(run) {
                        Some(result) => serde_json::json!({"available": true, "summary": result["summary"], "basis": result["basis"], "model": result["model"]}),
                        None => serde_json::json!({"available": false, "assess": format!("{} runs {} assertions assess", crate::launcher_command(), run.id)}),
                    });

                    let observed_kinds = data
                        .coverage_by_kind
                        .iter()
                        .filter_map(|entry| entry.kind.clone())
                        .collect::<Vec<_>>();
                    data.hints = full_suite_hints(root, &run.metadata.command, &observed_kinds);
                    // The copy the run's command ran in, while it is there.
                    data.workspace = run
                        .metadata
                        .copy
                        .and_then(|copy| {
                            supercov_engine::workspace::copy_path(root, copy).ok()
                        })
                        .filter(|path| path.is_dir())
                        .map(|path| path.display().to_string());
                }
                (output, warnings.clone())
            })
            .map_err(|error| PublicQueryExecutionError {
                error: Box::new(error),
                warnings,
            })
}

fn public_query_command(command: &str, mut arguments: Vec<String>) -> ExitCode {
    // `tests without-evidence` reads the run's index like any other query;
    // `tests affected` compares the run with the working tree.
    if command == "runs"
        && arguments.get(1).is_some_and(|a| a == "tests")
        && arguments.get(2).is_some_and(|a| a == "without-evidence")
    {
        arguments.remove(2);
        arguments[1] = "tests-without-evidence".into();
    }
    if command == "runs" && arguments.get(1).is_some_and(|a| a == "assertions") {
        return assertions_command::command(&arguments);
    }
    if command == "runs"
        && arguments.get(1).is_some_and(|a| a == "gaps")
        && arguments.iter().any(|a| a == "--format")
    {
        return gaps_export::command(&arguments);
    }
    if command == "runs" && arguments.get(1).is_some_and(|a| a == "source") {
        return source_query::command(&arguments);
    }
    if command == "runs" && arguments.get(1).is_some_and(|a| a == "tests") {
        return tests_query::command(&arguments);
    }
    // Both `runs check` (the latest run) and `runs <id> check` reach here.
    if command == "runs"
        && (arguments.first().is_some_and(|a| a == "check")
            || arguments.get(1).is_some_and(|a| a == "check"))
    {
        return coverage_check_command(&arguments);
    }
    if command == "runs"
        && (arguments.first().is_some_and(|a| a == "patch")
            || arguments.get(1).is_some_and(|a| a == "patch"))
    {
        return coverage_patch_command(&arguments);
    }
    if command == "runs"
        && (arguments.first().is_some_and(|a| a == "report")
            || arguments.get(1).is_some_and(|a| a == "report"))
    {
        return coverage_report_command(&arguments);
    }
    if let Some(help) = help_for(command, &arguments) {
        print!("{help}");
        return ExitCode::SUCCESS;
    }
    let invocation = match parse_public_query(command, &arguments) {
        Ok(invocation) => invocation,
        Err(error) => {
            if error.json {
                print!(
                    "{}",
                    agent_json::failure(error.command.as_deref(), &error.error)
                );
            } else {
                eprintln!("[supercov] {}", error.error.message);
            }
            return ExitCode::from(2);
        }
    };
    let (json_output, agent_command) = match &invocation {
        PublicQueryInvocation::Runs { json, .. } => (*json, "runs".to_owned()),
        PublicQueryInvocation::Coverage {
            json,
            agent_command,
            ..
        } => (*json, agent_command.clone()),
    };
    let root = match std::env::current_dir() {
        Ok(root) => root,
        Err(error) => {
            let error =
                internal_agent_error(format!("Could not resolve the current directory: {error}"));
            print!("{}", agent_json::failure(Some(&agent_command), &error));
            return ExitCode::from(2);
        }
    };
    match execute_public_query(&root, &invocation) {
        Ok(output) if json_output => {
            // A page too large for one reply is answered with the entries that
            // fit and the offset after them, rather than refused: a pager that
            // follows `nextOffset` reaches the end whatever its entries weigh.
            // Refusing meant a page of large entries stopped it, even at the
            // size the refusal had suggested for the page before.
            let mut invocation = invocation;
            let mut output = output;
            output.fit_lines();
            if let (
                PublicQueryInvocation::Coverage {
                    request,
                    newer_run_id,
                    ..
                },
                PublicQueryOutput::Coverage { output: page, .. },
            ) = (&mut invocation, &mut output)
            {
                let asked = request.limit;
                while let Some(fits) = page_that_fits(page) {
                    // What fits is fewer than the page returned, which is at
                    // most its limit; the limit falls each time, so this ends.
                    request.limit = fits.min(request.limit - 1);
                    match execute_coverage_query(&root, request, newer_run_id.as_deref()) {
                        Ok((smaller, _)) => {
                            *page = smaller;
                            record_page_cut(page, asked);
                        }
                        Err(failure) => {
                            print!(
                                "{}",
                                agent_json::failure(Some(&agent_command), &failure.error)
                            );
                            return ExitCode::from(2);
                        }
                    }
                }
                // A page cut to fit says so, in the reply and beside it: taken
                // for the whole of a file, page one hid most of its gaps.
                if request.limit < asked {
                    say_page_cut(page, asked);
                }
            }
            match output.agent_json() {
                Ok(output) => {
                    print!("{output}");
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    print!("{}", agent_json::failure(Some(&agent_command), &error));
                    ExitCode::from(2)
                }
            }
        }
        Ok(output) => {
            if let PublicQueryOutput::Coverage { warnings, .. } = &output {
                for warning in warnings {
                    eprintln!("{warning}");
                }
            }
            println!("{}", render_human(&output));
            ExitCode::SUCCESS
        }
        Err(failure) => {
            if !json_output {
                for warning in &failure.warnings {
                    eprintln!("{warning}");
                }
            }
            if json_output {
                print!(
                    "{}",
                    agent_json::failure(Some(&agent_command), &failure.error)
                );
            } else {
                eprintln!("[supercov] {}", failure.error.message);
            }
            ExitCode::from(2)
        }
    }
}

fn run_js_direct() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let mut request: supercov_engine::javascript_run::DirectJavascriptRunRequest =
        match serde_json::from_str(&input) {
            Ok(request) => request,
            Err(error) => {
                eprintln!("[supercov] invalid direct JavaScript run input: {error}");
                return ExitCode::from(2);
            }
        };
    request.watchdog_program = std::env::current_exe()
        .ok()
        .and_then(|path| fs::canonicalize(path).ok());
    let mut diagnostics = supercov_engine::fault::diagnostics();
    match supercov_engine::javascript_run::run_direct_javascript(&request, &mut diagnostics) {
        Ok(result) => {
            println!(
                "{}",
                serde_json::to_string(&result).expect("plain data serializes")
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] direct JavaScript run failed: {error}");
            ExitCode::from(2)
        }
    }
}

fn supervise() -> ExitCode {
    let mut arguments = std::env::args_os().skip(2).collect::<Vec<_>>();
    if arguments.first().is_some_and(|argument| argument == "--") {
        arguments.remove(0);
    }
    let Some(program) = arguments.first().cloned() else {
        eprintln!("[supercov] test command must not be empty");
        return ExitCode::from(2);
    };
    let diagnostic_interval = match supercov_engine::process_supervision::positive_milliseconds(
        std::env::var("SUPERCOV_DIAGNOSTIC_INTERVAL_MS")
            .ok()
            .as_deref(),
        "SUPERCOV_DIAGNOSTIC_INTERVAL_MS",
    ) {
        Ok(value) => value.unwrap_or_else(|| {
            std::time::Duration::from_millis(supercov_contracts::DEFAULT_DIAGNOSTIC_INTERVAL_MS)
        }),
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let timeout = match supercov_engine::process_supervision::positive_milliseconds(
        std::env::var("SUPERCOV_COMMAND_TIMEOUT_MS").ok().as_deref(),
        "SUPERCOV_COMMAND_TIMEOUT_MS",
    ) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(error) => {
            eprintln!("[supercov] could not resolve the current directory: {error}");
            return ExitCode::from(2);
        }
    };
    let spec = supercov_engine::process_supervision::CommandSpec {
        program,
        arguments: arguments.into_iter().skip(1).collect(),
        cwd,
        environment: None,
        captured_output: None,
        console: None,
    };
    let mut stderr = std::io::stderr().lock();
    let supervisor = match std::env::current_exe()
        .map_err(|error| error.to_string())
        .and_then(|path| {
            supercov_engine::process_supervision::ProcessSupervisor::new_crash_safe(&path)
                .map_err(|error| error.to_string())
        }) {
        Ok(supervisor) => supervisor,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::FAILURE;
        }
    };
    match supervisor.supervise(
        &spec,
        supercov_engine::process_supervision::SupervisionOptions {
            diagnostic_interval,
            timeout,
            termination_grace: std::time::Duration::from_millis(
                supercov_contracts::COMMAND_TERMINATION_GRACE_MS,
            ),
        },
        &mut stderr,
    ) {
        // A code no byte holds (Windows has them) is a failure.
        Ok(result) => ExitCode::from(u8::try_from(result.exit_code()).unwrap_or(1)),
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::FAILURE
        }
    }
}

/// How long a command waits for the trash to be empty before it goes on
/// without knowing: at its start, when what is there is another run's and
/// the work has yet to begin.
const TRASH_PATIENCE_AT_START: Duration = Duration::from_millis(100);
/// And at its end, when the next thing to run is the user's. Nothing of
/// Supercov's should still be changing the project once it has exited: a
/// tenth of a second was not that. The run empties most of the trash while
/// the command runs, so this is rarely waited for.
const TRASH_PATIENCE_AT_END: Duration = Duration::from_secs(10);

fn spawn_trash_sweeper(root: &Path) {
    let patience = supercov_engine::fault::or(
        "sweep-impatient",
        || Duration::ZERO,
        || TRASH_PATIENCE_AT_START,
    );
    let still_sweeping = sweep_trash_within(root, patience);
    *START_SWEEP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = still_sweeping;
}

/// The sweep started with the command, while it may still be running. It
/// holds the trash's lock, so a sweep started at the end would find it held
/// and leave at once: the end waits for this one instead.
static START_SWEEP: std::sync::Mutex<Option<std::process::Child>> = std::sync::Mutex::new(None);

/// Empty the trash before the command's own process ends.
fn finish_trash_sweep(root: &Path) {
    let patience = supercov_engine::fault::or(
        "sweep-impatient",
        || Duration::ZERO,
        || TRASH_PATIENCE_AT_END,
    );
    let deadline = Instant::now() + patience;
    supercov_engine::lifecycle::finish_background_sweep(patience);
    let start = START_SWEEP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let still_sweeping = start.and_then(|child| {
        // A test makes it still be at it, however fast the trash empties.
        if supercov_engine::fault::injected("sweep-outlasts") {
            Some(child)
        } else {
            wait_until(child, deadline)
        }
    });
    if still_sweeping.is_some() {
        // Still at it: it goes on by itself, as a sweep given up on does.
        return;
    }
    sweep_trash_within(root, deadline.saturating_duration_since(Instant::now()));
}

/// Starts a sweep and gives it until `patience` runs out; the sweep, if it
/// is still running then.
fn sweep_trash_within(root: &Path, patience: Duration) -> Option<std::process::Child> {
    let executable = std::env::current_exe().ok()?;
    let mut command = Command::new(executable);
    command
        .arg("__sweep-trash")
        .arg(root)
        // Keep the best-effort child from pinning a project or isolated
        // workspace as its cwd. This is required for recursive cleanup on
        // Windows and harmless on Unix filesystems.
        .current_dir(std::env::temp_dir())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.launch().ok()?;
    // The sweep is given until `patience` runs out to finish, so that a
    // command launched right after Supercov does not find the trash on its
    // way out. Past that it goes on by itself: the independent process group
    // lets the child survive the parent CLI exiting on Unix.
    wait_until(child, Instant::now() + patience)
}

/// The child, if it is still running at `deadline`.
fn wait_until(mut child: std::process::Child, deadline: Instant) -> Option<std::process::Child> {
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= deadline => return Some(child),
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}

/// `__source-tool <tool> <arguments>`: what a launcher in an instrumented
/// workspace's `.bin` runs in place of a tool that judges source as text.
fn source_tool() -> ExitCode {
    let mut arguments = std::env::args_os().skip(2);
    let Some(tool) = arguments.next().map(PathBuf::from) else {
        return ExitCode::from(2);
    };
    let code =
        supercov_engine::source_tools::run_source_tool(&tool, &arguments.collect::<Vec<_>>());
    ExitCode::from(code.clamp(0, 255) as u8)
}

fn sweep_trash() -> ExitCode {
    let Some(root) = std::env::args_os().nth(2).map(PathBuf::from) else {
        return ExitCode::from(2);
    };
    match supercov_engine::lifecycle::sweep_trash(&root) {
        Ok(_) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(2),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredQueryRequest {
    root: PathBuf,
    query: IndexedQueryRequest,
    newer_run_id: Option<String>,
}

/// A stored run's coverage, of only the tests `filter` picks when it is given.
fn analyze_stored_run_filtered(
    run: &StoredRun,
    filter: Option<&supercov_engine::coverage_report::TestFilter>,
) -> Result<supercov_engine::coverage_report::CoverageReport, String> {
    analyze_coverage_archive(&ArchiveReportRequest {
        archive_path: run.evidence_path.clone(),
        run_id: run.id.clone(),
        generated_at: run.metadata.started_at.clone(),
        integrity: serde_json::to_value(&run.metadata.integrity).ok(),
        test_exit_code: supercov_engine::coverage_report::ExitCodeInput::Present(
            run.metadata.test_exit_code,
        ),
        test_filter: filter.cloned(),
    })
    .map_err(|error| match error {
        ReportError::NoTestMatches(filter) => format!("no test of the run matches {filter}"),
        error => format!("invalid coverage archive: {error:?}"),
    })
}

/// Internal differential surface for real persisted-run opening and indexing.
/// Public argument parsing is enabled only after this path has exact parity.
fn query_stored_run() -> ExitCode {
    let input = match stdin() {
        Ok(input) => input,
        Err(error) => {
            eprintln!("[supercov] {error}");
            return ExitCode::from(2);
        }
    };
    let mut request: StoredQueryRequest = match serde_json::from_str(&input) {
        Ok(request) => request,
        Err(error) => {
            eprintln!("[supercov] invalid stored query input: {error}");
            return ExitCode::from(2);
        }
    };
    let result = (|| -> Result<String, String> {
        let inventory = discover_runs(&request.root).map_err(|error| error.to_string())?;
        let run = select_run(&inventory, Some(&request.query.run_id))
            .map_err(|error| error.to_string())?;
        request.query.run_id.clone_from(&run.id);
        request
            .query
            .valid
            .get_or_insert(run.metadata.test_exit_code == Some(0));
        let test_filter = request.query.test_filter();
        let container =
            open_query_index_for(run, test_filter.as_ref()).map_err(|error| error.to_string())?;
        let index = CoverageIndex::new(&container).map_err(|error| error.to_string())?;
        let report = if request.query.command == "minimize" {
            Some(analyze_stored_run_filtered(run, test_filter.as_ref())?)
        } else {
            None
        };

        let newer_container;
        let newer_index;
        let newer_query = if request.query.command == "diff" {
            let selector = request
                .newer_run_id
                .as_deref()
                .ok_or_else(|| "stored diff requires a newer run ID".to_owned())?;
            let newer_run =
                select_run(&inventory, Some(selector)).map_err(|error| error.to_string())?;
            newer_container = open_query_index_for(newer_run, test_filter.as_ref())
                .map_err(|error| error.to_string())?;
            newer_index =
                CoverageIndex::new(&newer_container).map_err(|error| error.to_string())?;
            Some(NewerQuery {
                run_id: &newer_run.id,
                index: &newer_index,
            })
        } else {
            None
        };
        execute_indexed_query(&index, report.as_ref(), &request.query, newer_query)
            .map_err(|error| error.to_string())
    })();
    match result {
        Ok(output) => {
            print!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("[supercov] stored query failed: {error}");
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
    if let Err(error) = write_json_to_stdout(&result) {
        eprintln!("[supercov] failed to write Rust benchmark output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

/// Writes and flushes: an error the buffer would only meet at exit is lost.
fn write_json_to_stdout(value: &impl Serialize) -> std::io::Result<()> {
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, value)?;
    stdout.flush()
}

fn stdin() -> Result<String, String> {
    let input = stdin_bytes()?;
    String::from_utf8(input).map_err(|error| format!("Rust engine input is not UTF-8: {error}"))
}

fn stdin_bytes() -> Result<Vec<u8>, String> {
    if let Some(path) = std::env::var_os("SUPERCOV_INTERNAL_INPUT_FILE") {
        return fs::read(&path).map_err(|error| {
            format!(
                "failed to read Rust engine input {}: {error}",
                Path::new(&path).display()
            )
        });
    }
    let mut input = Vec::new();
    std::io::stdin()
        .read_to_end(&mut input)
        .map_err(|error| format!("failed to read Rust engine input: {error}"))?;
    Ok(input)
}

#[derive(Deserialize)]
struct InstrumentCase {
    file: String,
    source: String,
}

/// Private batch protocol used by conformance and performance harnesses.
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
    if let Err(error) = write_json_to_stdout(&outputs) {
        eprintln!("[supercov] failed to write Rust instrumenter output: {error}");
        return ExitCode::from(2);
    }
    ExitCode::SUCCESS
}

#[cfg(test)]
mod docs_command_tests {
    use super::localize_guide;

    const GUIDE: &str = "Run `npx supercov security` or `supercov quality`.\nKeep `supercov-cli`, `npx supercov-cli` and `supercov`.\n\n```sh\nnpx supercov -- npm test\nsupercov runs latest   # newest run\n$ supercov security patch\n```\n\n```text\nsupercov security: 3 files flagged\n```\n";

    #[test]
    fn guides_print_commands_the_way_the_reader_started_supercov() {
        let go = "go run github.com/supercorp-ai/supercov/cmd/supercov@latest";
        let printed = localize_guide(GUIDE, go);
        assert!(printed.contains(&format!("Run `{go} security` or `{go} quality`.")));
        assert!(printed.contains(&format!(
            "{go} -- npm test\n{go} runs latest   # newest run\n$ {go} security patch"
        )));
        // A package name, the bare binary name and recorded output stay put.
        assert!(printed.contains("Keep `supercov-cli`, `npx supercov-cli` and `supercov`."));
        assert!(printed.contains("```text\nsupercov security: 3 files flagged\n```"));

        let npm = localize_guide(GUIDE, "npx supercov");
        assert!(npm.contains("`npx supercov quality`"));
        assert!(npm.contains("npx supercov runs latest"));
        assert!(!npm.contains("npx npx"));

        let native = localize_guide(GUIDE, "supercov");
        assert!(native.contains("`supercov security`"));
        assert!(native.contains("```sh\nsupercov -- npm test"));
        assert_eq!(localize_guide("", "supercov"), "");
    }
}

/// Why a run that published no evidence has none, by how its command
/// exited.
fn no_evidence_message(exit: Option<i32>) -> String {
    match exit {
        // A command that passed with nothing recorded ran no test: a filter
        // that matched none, most often.
        Some(0) => "The wrapped command passed and ran no test Supercov could record, so there is no coverage to read; check that its test filter matches a test.".into(),
        Some(code) => format!(
            "The wrapped command exited {code} before publishing queryable coverage evidence. Inspect that command's original output and rerun after it passes."
        ),
        None => "The wrapped command ended without an exit status or queryable coverage evidence. Inspect that command's original output and rerun.".into(),
    }
}

#[cfg(test)]
mod tests {
    /// A run with no evidence is told apart by how its command exited.
    #[test]
    fn a_run_without_evidence_says_how_its_command_ended() {
        assert!(super::no_evidence_message(Some(0)).contains("ran no test"));
        assert!(super::no_evidence_message(Some(3)).contains("exited 3"));
        assert!(super::no_evidence_message(None).contains("without an exit status"));
    }

    /// The contract version each frontend actually records, so this cannot
    /// drift from the strings runs are stored with.
    #[test]
    fn every_frontend_answers_for_its_own_runs() {
        for (version, expected) in [
            ("supercov-rust-rust-owned-v1", Frontend::Rust),
            ("supercov-ruby-ruby-owned-v1", Frontend::Ruby),
            ("supercov-python-python-owned-v1", Frontend::Python),
            ("supercov-go-go-owned-v1", Frontend::Go),
            ("supercov-jvm-jvm-owned-v1", Frontend::Jvm),
            ("javascript-v1", Frontend::JavaScript),
        ] {
            assert_eq!(frontend_of(version), expected, "{version}");
        }
        // A frontend nobody has written yet must not quietly become
        // JavaScript's problem in a way that reads as a real answer -- but
        // JavaScript is what a run predating these prefixes recorded, so it
        // stays the fallback.
        assert_eq!(frontend_of("supercov-zig-v1"), Frontend::JavaScript);
    }

    use super::*;

    /// A run that measured nothing exits as what stopped it: 2 for a store
    /// being cleaned, the command's own status when it ended before any test,
    /// a shell's 127 and 126 for a program missing or not allowed to run,
    /// and 1 otherwise.
    #[test]
    fn a_run_that_measured_nothing_exits_as_what_stopped_it() {
        for (error, code) in [
            ("the project's storage is being cleaned; try again", 2),
            (
                "the test command exited 3 before any test reported an outcome; its own output above says why",
                3,
            ),
            (
                "the test command exited x before any test reported an outcome",
                1,
            ),
            ("the test command exited 3 after a while", 1),
            (
                "could not spawn nope: no such program: No such file or directory (os error 2)",
                127,
            ),
            (
                "could not spawn ./t.sh: not allowed to run: Permission denied (os error 13)",
                126,
            ),
            ("could not spawn x: other (os error 5)", 1),
            ("something else", 1),
        ] {
            assert_eq!(could_not_measure(error), ExitCode::from(code), "{error}");
        }
    }

    #[test]
    fn the_timings_line_reports_publication_and_the_whole_command() {
        let timings = supercov_engine::run_store::RunTimings {
            initialization_ms: 1.0,
            workspace_preparation_ms: 2.0,
            adapter_setup_ms: 3.0,
            instrumented_build_ms: 4.0,
            test_command_ms: 5.0,
            evidence_publication_ms: 6.0,
        };
        // Published: the command ran 30 ms, 21 of them before the engine's
        // clock stopped, so publication took the other 9 and total is 30.
        assert_eq!(
            format_run_timings(&timings, 21.0, Some(30.0)),
            "initialization=1ms workspace=2ms setup=3ms build=4ms tests=5ms evidence=6ms publication=9ms total=30ms"
        );
        // A clock that reads a hair under the engine's never reports negative
        // publication or a total below what the engine measured.
        assert_eq!(
            format_run_timings(&timings, 21.0, Some(20.9)),
            "initialization=1ms workspace=2ms setup=3ms build=4ms tests=5ms evidence=6ms publication=0ms total=21ms"
        );
        // Interrupted: nothing was published, and the line says only that.
        assert_eq!(
            format_run_timings(&timings, 21.0, None),
            "initialization=1ms workspace=2ms setup=3ms build=4ms tests=5ms evidence=6ms total=21ms"
        );
    }

    #[test]
    fn shell_reports_the_public_engine() {
        assert!(HELP.contains("Coverage, security and code quality for coding agents"));
        assert!(HELP.contains("full test command"));
        assert!(HELP.contains("npx supercov -- npm test"));
        assert!(HELP.contains("supercov docs"));
        // Quality, coverage, then assertions, each in its own section.
        let order = [
            "Score the source with Jev:",
            "Measure your full test command:",
            "Inspect a run with small, paginated answers:",
            "Check what the tests assert:",
        ];
        let mut at = 0;
        for needle in order {
            let found = HELP[at..].find(needle).expect(needle);
            at += found + needle.len();
        }
        assert!(!HELP.contains("Assertion map schema and syntax: supercov assertions"));
    }

    /// The package script a command runs, however npm, pnpm or Yarn is
    /// asked, and none for anything else.
    #[test]
    fn the_package_script_a_command_runs_is_read_from_it() {
        let script = |command: &[&str]| {
            let command = command
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>();
            selected_package_script(&command).map(str::to_owned)
        };
        assert_eq!(script(&[]), None);
        assert_eq!(script(&["node", "--test"]), None);
        assert_eq!(script(&["npm", "-s", "t"]).as_deref(), Some("test"));
        assert_eq!(
            script(&["npm", "run", "--silent", "test:unit"]).as_deref(),
            Some("test:unit")
        );
        assert_eq!(script(&["pnpm", "test:e2e"]).as_deref(), Some("test:e2e"));
        assert_eq!(script(&["npm", "install"]), None);
    }

    #[test]
    fn suite_slice_hint_points_to_the_existing_full_package_command() {
        let root = std::env::temp_dir().join(format!(
            "supercov-suite-hint-{}-{}",
            std::process::id(),
            OffsetDateTime::now_utc().unix_timestamp_nanos()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"scripts":{"test":"npm run test:unit && npm run test:e2e","test:unit":"vitest run","test:e2e":"playwright test"}}"#,
        )
        .unwrap();
        let hints = full_suite_hints(
            &root,
            &["npm".into(), "run".into(), "test:unit".into()],
            &["unit".into()],
        );
        assert_eq!(hints.len(), 1);
        assert!(hints[0].contains("only the `unit` test kind"));
        assert!(hints[0].contains("test:e2e"));
        assert!(hints[0].contains(&format!("{} -- npm test", launcher_command())));
        // A package file that is not JSON, or names no scripts, hints nothing.
        for package in ["{ not json", r#"{"name":"p"}"#] {
            fs::write(root.join("package.json"), package).unwrap();
            let hints = full_suite_hints(
                &root,
                &["npm".into(), "run".into(), "test:unit".into()],
                &["unit".into()],
            );
            assert!(hints.is_empty(), "{package}");
        }
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn cleanup_options_preserve_the_frozen_cli_contract() {
        assert_eq!(parse_cleanup_options(&[]).unwrap(), (0, false));
        assert_eq!(
            parse_cleanup_options(&["--keep".into(), "20".into(), "--dry-run".into()]).unwrap(),
            (20, true)
        );
        assert_eq!(
            parse_cleanup_options(&["--keep".into()]).unwrap_err(),
            "--keep must be a non-negative integer"
        );
        assert_eq!(
            parse_cleanup_options(&["--unknown".into()]).unwrap_err(),
            "Unknown clean option: --unknown"
        );
        let result = supercov_engine::lifecycle::CleanupResult {
            removed_runs: vec!["run-1".into(), "run-0".into()],
            removed_workspaces: vec!["run-1".into()],
            removed_evidence: vec![],
            removed_failed_evidence: vec![],
            removed_build_cache: false,
        };
        assert_eq!(
            cleanup_summary(0, false, &result),
            "[supercov] removed 2 stored run(s), 1 per-run workspace(s), and no isolated build cache; keeping 0 newest run(s)"
        );
        // Kept evidence is the one thing here a user cannot get back, so the
        // line says it went rather than leaving it to the run count.
        let with_kept = supercov_engine::lifecycle::CleanupResult {
            removed_failed_evidence: vec!["run_kept".into()],
            ..result
        };
        assert_eq!(
            cleanup_summary(0, false, &with_kept),
            "[supercov] removed 2 stored run(s), 1 per-run workspace(s), the kept evidence of 1 failed run(s), and no isolated build cache; keeping 0 newest run(s)"
        );
    }

    /// A file that cannot be made, written or put in place says which, and
    /// leaves no temporary file behind.
    #[test]
    fn an_atomic_write_that_fails_says_where() {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-cli-atomic-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("file"), "a file").unwrap();
        let refused = write_atomically(&root.join("file/report.json"), "{}", false).unwrap_err();
        assert!(refused.starts_with("could not create "), "{refused}");
        let occupied = root.join("occupied.json");
        fs::create_dir_all(
            occupied.with_extension(format!("json.supercov-{}", std::process::id())),
        )
        .unwrap();
        let refused = write_atomically(&occupied, "{}", false).unwrap_err();
        assert!(refused.starts_with("could not write "), "{refused}");
        let directory = root.join("directory");
        fs::create_dir_all(directory.join("inside")).unwrap();
        let refused = write_atomically(&directory, "{}", true).unwrap_err();
        assert!(refused.starts_with("could not replace "), "{refused}");
        assert!(
            !root
                .join(format!("directory.tmp.supercov-{}", std::process::id()))
                .exists()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn public_run_ids_are_short_opaque_and_unique() {
        let (first, first_started_at) = public_run_identity();
        let (second, second_started_at) = public_run_identity();
        assert!(first.starts_with("run_"));
        assert_eq!(first.len(), 20);
        assert!(public_run_id(&first));
        assert_ne!(first, second);
        assert!(!public_run_id("2026-08-25T23-33-12-211Z"));
        assert!(!public_run_id("run_35c35ceeaf4b843Z"));
        assert!(!public_run_id("abc_35c35ceeaf4b8430"));
        assert!(first_started_at.contains('T'));
        assert!(second_started_at.contains('T'));
    }
}
