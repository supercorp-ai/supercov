use serde_json::json;
use supercov_engine::{
    agent_json::{AgentError, ErrorCode},
    coverage_query::{DecisionSort, MinimizeMetric},
    indexed_query::IndexedQueryRequest,
};

const DEFAULT_LIMIT: usize = 20;
const DEFAULT_MAX_STATES: usize = 5_000;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub fn help_for(command: &str, arguments: &[String]) -> Option<String> {
    if !arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        return None;
    }
    if command == "diff" {
        return Some(
            "Usage: supercov diff <older-run> <newer-run> [options]\n\nOptions:\n  --filter <all|passed|failed>\n  --kind <kind>\n  --runner <runner>\n  --tag <tag>                    only tests with this tag (@e2e, a pytest marker, a JUnit @Tag); repeat for any of several; \'!api\' leaves those out\n  --title <text>                 only tests whose name contains this\n  --test-file <path>             only tests in this file or directory\n  --metric <metric>\n  --gained                       only what the newer run covers that the older did not\n  --offset <n>\n  --limit <n>                    entries per page (default 20; 0 for all); a page names the --limit that shows all\n  --json\n"
                .into(),
        );
    }
    let run = arguments
        .first()
        .filter(|argument| !argument.starts_with('-'));
    let Some(_) = run else {
        return Some(
            "Usage: supercov runs [options]\n       supercov runs <run-id> [query]\n\nLists runs and lazily calculates their coverage summaries.\n\nOptions:\n  --filter <all|passed|failed>\n  --offset <n>\n  --limit <n>                    entries per page (default 20; 0 for all); a page names the --limit that shows all\n  --json\n"
                .into(),
        );
    };
    let child = arguments
        .get(1)
        .filter(|argument| !argument.starts_with('-'))
        .map(String::as_str);
    let Some(child) = child else {
        return Some(
            "Usage: supercov runs <run-id> [query] [options]\n\nWithout a query, prints the run's coverage summary.\n\nQueries:\n  files                 every included file, including fully covered files; --group dir for coverage by directory\n  gaps                  only files with unresolved coverage or measurement gaps\n  file <path>           gap lines with consolidated obligations\n  line <file:line>      line state, obligations, tests and phases\n  decision <location>   MC/DC vectors and witness pairs for one decision\n  test <id|name>        coverage attributed to one test\n  kinds                 coverage grouped by unit/component/integration/e2e\n  runners               coverage grouped by test runner\n  scope                 what is source and what is not, by directory; --files lists every file\n  minimize              smallest test set for a coverage target\n  tests affected        tests the changes since the run could have reached\n  tests without-evidence  tests that made assertions and recorded no coverage\n  assertions            how much executed code the tests assert; see runs <run-id> assertions --help\n\nCommon options:\n  --filter <all|passed|failed>   recalculate coverage from those attempts only\n  --kind <kind>\n  --runner <runner>\n  --tag <tag>                    only tests with this tag (@e2e, a pytest marker, a JUnit @Tag); repeat for any of several; \'!api\' leaves those out\n  --title <text>                 only tests whose name contains this\n  --test-file <path>             only tests in this file or directory\n  --offset <n>\n  --limit <n>                    entries per page (default 20; 0 for all); a page names the --limit that shows all\n  --json\n\nRun any query with --help for its exact usage.\n"
                .replace("  minimize              ", "  source <path>         matching current source with line numbers\n  minimize              "),
        );
    };
    let usage = match child {
        "summary" => {
            "supercov runs <run-id> [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--json]"
        }
        "files" => {
            "supercov runs <run-id> files [--group dir [--depth <n>]] [--metric <all|lines|statements|functions|branches|mcdc>] [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--offset <n>] [--limit <n>] [--json]"
        }
        "gaps" => {
            "supercov runs <run-id> gaps [--metric <all|lines|statements|functions|branches|mcdc>] [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--offset <n>] [--limit <n>] [--json]\n       supercov runs <run-id> gaps --format jsonl [--metric <metric>] [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--output <file>]"
        }
        "tests-without-evidence" => {
            "supercov runs <run-id> tests without-evidence [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--offset <n>] [--limit <n>] [--json]"
        }
        "kinds" => {
            "supercov runs <run-id> kinds [--filter <all|passed|failed>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--offset <n>] [--limit <n>] [--json]"
        }
        "runners" => {
            "supercov runs <run-id> runners [--filter <all|passed|failed>] [--kind <kind>] [--offset <n>] [--limit <n>] [--json]"
        }
        "scope" => "supercov runs <run-id> scope [--files] [--offset <n>] [--limit <n>] [--json]",
        "file" => {
            "supercov runs <run-id> file <path> [--group decision] [--sort <location|missing>] [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--offset <n>] [--limit <n>] [--json]"
        }
        "decision" => {
            "supercov runs <run-id> decision <id|source-file:line> [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--json]"
        }
        "line" => {
            "supercov runs <run-id> line <source-file:line> [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--offset <n>] [--limit <n>] [--json]"
        }
        "test" => {
            "supercov runs <run-id> test <id|name-fragment> [--filter <all|passed|failed>] [--json]"
        }
        "minimize" => {
            "supercov runs <run-id> minimize [--target <0..100>] [--metric <all|lines|statements|functions|branches|mcdc>] [--filter <all|passed|failed>] [--kind <kind>] [--runner <runner>] [--tag <tag>] [--title <text>] [--test-file <path>] [--json]"
        }
        _ => "supercov runs <run-id> --help",
    };
    Some(format!("Usage: {usage}\n"))
}

#[derive(Debug)]
pub struct PublicQueryError {
    pub command: Option<String>,
    pub json: bool,
    pub error: Box<AgentError>,
}

impl PublicQueryError {
    fn invalid(command: Option<&str>, json_output: bool, message: impl Into<String>) -> Self {
        Self {
            command: command.map(str::to_owned),
            json: json_output,
            error: Box::new(AgentError {
                code: ErrorCode::InvalidArgument,
                message: message.into(),
                retryable: false,
                details: None,
            }),
        }
    }

    fn unknown(
        command: Option<&str>,
        json_output: bool,
        message: impl Into<String>,
        details: serde_json::Value,
    ) -> Self {
        Self {
            command: command.map(str::to_owned),
            json: json_output,
            error: Box::new(AgentError {
                code: ErrorCode::UnknownCommand,
                message: message.into(),
                retryable: false,
                details: Some(details),
            }),
        }
    }
}

#[derive(Debug)]
pub enum PublicQueryInvocation {
    Runs {
        filter: String,
        offset: usize,
        limit: usize,
        json: bool,
    },
    Coverage {
        request: Box<IndexedQueryRequest>,
        newer_run_id: Option<String>,
        agent_command: String,
        json: bool,
    },
}

#[derive(Debug)]
struct QueryOptions {
    /// `line --from-file <path>`: locations to ask, one per line.
    from_file: Option<String>,
    run: Option<String>,
    kind: Option<String>,
    runner: Option<String>,
    /// `--tag`, `--title`, `--test-file`: tests picked by what they carry.
    tags: Vec<String>,
    titles: Vec<String>,
    test_files: Vec<String>,
    filter: String,
    limit: usize,
    offset: usize,
    json: bool,
    target: Option<f64>,
    metric: MinimizeMetric,
    group_decision: bool,
    /// `scope --files`: the scope file by file.
    scope_files: bool,
    /// `files --group dir`: files rolled up by directory.
    group_dir: bool,
    depth: Option<usize>,
    /// `diff --gained`.
    gained: bool,
    sort: DecisionSort,
    positional: Vec<String>,
}

impl Default for QueryOptions {
    fn default() -> Self {
        Self {
            from_file: None,
            run: None,
            kind: None,
            runner: None,
            tags: Vec::new(),
            titles: Vec::new(),
            test_files: Vec::new(),
            filter: "all".into(),
            limit: DEFAULT_LIMIT,
            offset: 0,
            json: false,
            target: None,
            metric: MinimizeMetric::All,
            group_decision: false,
            scope_files: false,
            group_dir: false,
            depth: None,
            gained: false,
            sort: DecisionSort::Location,
            positional: Vec::new(),
        }
    }
}

fn parse_unsigned(value: Option<&String>, positive: bool) -> Option<usize> {
    let value = value?;
    let parsed = value.parse::<f64>().ok()?;
    if !parsed.is_finite()
        || parsed.fract() != 0.0
        || parsed < 0.0
        || parsed > MAX_SAFE_INTEGER as f64
        || (positive && parsed == 0.0)
    {
        return None;
    }
    Some(parsed as usize)
}

fn parse_options(
    command: &str,
    arguments: &[String],
    json_output: bool,
) -> Result<QueryOptions, PublicQueryError> {
    let agent_command = if matches!(command, "runs" | "diff" | "help") {
        command.to_owned()
    } else {
        format!("coverage.{command}")
    };
    let mut options = QueryOptions {
        json: json_output,
        ..QueryOptions::default()
    };
    let mut index = 0;
    while index < arguments.len() {
        let value = &arguments[index];
        match value.as_str() {
            "--json" => options.json = true,
            "--run" => {
                index += 1;
                let Some(run) = arguments.get(index) else {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--run requires a run ID",
                    ));
                };
                options.run = Some(run.clone());
            }
            "--kind" => {
                index += 1;
                let Some(kind) = arguments.get(index) else {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--kind requires a test kind",
                    ));
                };
                options.kind = Some(kind.to_lowercase());
            }
            "--runner" => {
                index += 1;
                let Some(runner) = arguments.get(index) else {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--runner requires a runner name",
                    ));
                };
                options.runner = Some(runner.to_lowercase());
            }
            flag @ ("--tag" | "--title" | "--test-file") => {
                index += 1;
                let Some(value) = arguments.get(index).filter(|value| !value.is_empty()) else {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        match flag {
                            "--tag" => "--tag requires a tag, such as @e2e",
                            "--title" => "--title requires text the test's name contains",
                            _ => "--test-file requires a test file or directory",
                        },
                    ));
                };
                match flag {
                    "--tag" => options.tags.push(value.clone()),
                    "--title" => options.titles.push(value.clone()),
                    _ => options.test_files.push(value.clone()),
                }
            }
            "--filter" => {
                index += 1;
                let filter = arguments.get(index).map(|filter| filter.to_lowercase());
                if !matches!(filter.as_deref(), Some("all" | "passed" | "failed")) {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--filter must be all, passed, or failed",
                    ));
                }
                options.filter = filter.expect("validated coverage filter");
            }
            "--target" => {
                index += 1;
                let target = arguments
                    .get(index)
                    .and_then(|value| value.parse::<f64>().ok());
                if !target
                    .is_some_and(|target| target.is_finite() && (0.0..=100.0).contains(&target))
                {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--target must be between 0 and 100",
                    ));
                }
                options.target = target;
            }
            "--metric" => {
                index += 1;
                options.metric = match arguments.get(index).map(|metric| metric.to_lowercase()) {
                    Some(metric) if metric == "all" => MinimizeMetric::All,
                    Some(metric) if metric == "lines" => MinimizeMetric::Lines,
                    Some(metric) if metric == "statements" => MinimizeMetric::Statements,
                    Some(metric) if metric == "functions" => MinimizeMetric::Functions,
                    Some(metric) if metric == "branches" => MinimizeMetric::Branches,
                    Some(metric) if metric == "mcdc" => MinimizeMetric::Mcdc,
                    _ => {
                        return Err(PublicQueryError::invalid(
                            Some(&agent_command),
                            json_output,
                            "--metric must be all, lines, statements, functions, branches, or mcdc",
                        ));
                    }
                };
            }
            "--limit" => {
                index += 1;
                // `--limit 0` is every entry, as `assertions`, `quality` and
                // `security` spell it.
                let Some(limit) = parse_unsigned(arguments.get(index), false) else {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--limit must be a number of entries, or 0 for all of them",
                    ));
                };
                options.limit = if limit == 0 { usize::MAX } else { limit };
            }
            "--offset" => {
                index += 1;
                let Some(offset) = parse_unsigned(arguments.get(index), false) else {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--offset must be a non-negative integer",
                    ));
                };
                options.offset = offset;
            }
            "--files" => {
                if command != "scope" {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--files is only supported by: supercov runs <run-id> scope",
                    ));
                }
                options.scope_files = true;
            }
            "--from-file" => {
                index += 1;
                let Some(path) = arguments.get(index) else {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--from-file needs a file of <source-file>:<line> locations, one per line",
                    ));
                };
                options.from_file = Some(path.clone());
            }
            "--group" => {
                index += 1;
                let group = arguments.get(index).map(|group| group.to_lowercase());
                match (command, group.as_deref()) {
                    ("file", Some("decision")) => options.group_decision = true,
                    ("files", Some("dir")) => options.group_dir = true,
                    ("file", _) => {
                        return Err(PublicQueryError::invalid(
                            Some(&agent_command),
                            json_output,
                            "--group must be decision",
                        ));
                    }
                    ("files", _) => {
                        return Err(PublicQueryError::invalid(
                            Some(&agent_command),
                            json_output,
                            "--group must be dir",
                        ));
                    }
                    _ => {
                        return Err(PublicQueryError::invalid(
                            Some(&agent_command),
                            json_output,
                            "--group is only supported by: supercov runs <run-id> file <source-file> --group decision, and supercov runs <run-id> files --group dir",
                        ));
                    }
                }
            }
            "--gained" => options.gained = true,
            "--depth" => {
                index += 1;
                let Some(depth) = parse_unsigned(arguments.get(index), true) else {
                    return Err(PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        "--depth must be a positive integer",
                    ));
                };
                options.depth = Some(depth);
            }
            "--sort" => {
                index += 1;
                options.sort = match arguments.get(index).map(|sort| sort.to_lowercase()) {
                    Some(sort) if sort == "location" => DecisionSort::Location,
                    Some(sort) if sort == "missing" => DecisionSort::Missing,
                    _ => {
                        return Err(PublicQueryError::invalid(
                            Some(&agent_command),
                            json_output,
                            "--sort must be location or missing",
                        ));
                    }
                };
            }
            // A single dash is an option too (`-q`), except in a test name.
            option
                if option.starts_with("--")
                    || (option.starts_with('-') && option.len() > 1 && command != "test") =>
            {
                let mut error = PublicQueryError::invalid(
                    Some(&agent_command),
                    json_output,
                    format!("Unknown option: {option}"),
                );
                error.error.details = Some(json!({ "option": option }));
                return Err(error);
            }
            _ => options.positional.push(value.clone()),
        }
        index += 1;
    }
    if options.sort != DecisionSort::Location && !options.group_decision {
        return Err(PublicQueryError::invalid(
            Some(&agent_command),
            json_output,
            "--sort requires --group decision",
        ));
    }
    if options.depth.is_some() && !options.group_dir {
        return Err(PublicQueryError::invalid(
            Some(&agent_command),
            json_output,
            "--depth requires --group dir",
        ));
    }
    if options.group_decision
        && !matches!(options.metric, MinimizeMetric::All | MinimizeMetric::Mcdc)
    {
        return Err(PublicQueryError::invalid(
            Some(&agent_command),
            json_output,
            "--group decision lists MC/DC decisions; omit --metric or use --metric mcdc",
        ));
    }
    Ok(options)
}

/// `file:line` or `file:line:column`; lines count from 1.
fn parse_location(selector: &str) -> Option<(String, usize)> {
    let without_column = selector.rsplit_once(':')?;
    let (file_and_maybe_line, final_number) = without_column;
    let final_number = final_number.parse::<usize>().ok()?;
    if let Some((file, line)) = file_and_maybe_line.rsplit_once(':')
        && let Ok(line) = line.parse::<usize>()
    {
        return (line > 0).then(|| (file.to_owned(), line));
    }
    (final_number > 0).then(|| (file_and_maybe_line.to_owned(), final_number))
}

fn coverage_invocation(
    run_id: String,
    command: &str,
    arguments: &[String],
    json_output: bool,
) -> Result<PublicQueryInvocation, PublicQueryError> {
    let (request, json) = coverage_request(run_id, command, arguments, json_output)?;
    Ok(PublicQueryInvocation::Coverage {
        request: Box::new(request),
        newer_run_id: None,
        agent_command: format!("coverage.{command}"),
        json,
    })
}

/// The request `runs <run> <command> <arguments>` makes of the run's index,
/// and whether its answer is asked for as JSON.
pub(crate) fn coverage_request(
    run_id: String,
    command: &str,
    arguments: &[String],
    json_output: bool,
) -> Result<(IndexedQueryRequest, bool), PublicQueryError> {
    let options = parse_options(command, arguments, json_output)?;
    let agent_command = format!("coverage.{command}");
    let mut request = IndexedQueryRequest {
        run_id,
        filter: options.filter,
        command: command.into(),
        metric: options.metric,
        kind: options.kind,
        runner: options.runner,
        file: None,
        line: None,
        selector: None,
        sort: None,
        valid: None,
        test_exit_code: None,
        stale: None,
        stale_reasons: None,
        offset: options.offset,
        limit: options.limit,
        target: None,
        max_states: None,
        group: None,
        depth: None,
        gained_only: false,
        more_lines: Vec::new(),
        tags: options.tags.clone(),
        titles: options.titles.clone(),
        test_files: options.test_files.clone(),
    };
    // A word a query does not take would be read as a filter it is not.
    let takes = match command {
        "file" | "decision" => 1,
        "line" | "test" => usize::MAX,
        _ => 0,
    };
    if let Some(extra) = options.positional.get(takes) {
        let mut error = PublicQueryError::invalid(
            Some(&agent_command),
            json_output,
            format!("Unexpected argument: {extra}"),
        );
        error.error.details = Some(json!({ "argument": extra }));
        return Err(error);
    }
    match command {
        "summary" | "kinds" | "runners" | "gaps" | "tests-without-evidence" => {}
        "files" => {
            if options.group_dir {
                request.group = Some("dir".into());
                request.depth = options.depth;
            }
        }
        "scope" => {
            if options.scope_files {
                request.group = Some("file".into());
            }
        }
        "file" => {
            let Some(file) = options.positional.first() else {
                return Err(PublicQueryError::invalid(
                    Some(&agent_command),
                    json_output,
                    "Usage: supercov runs <run-id> file <source-file>",
                ));
            };
            request.file = Some(file.clone());
            if options.group_decision {
                request.command = "file-decisions".into();
                request.sort = Some(options.sort);
            } else {
                request.command = "file-detail".into();
            }
        }
        "decision" => {
            let Some(selector) = options.positional.first() else {
                return Err(PublicQueryError::invalid(
                    Some(&agent_command),
                    json_output,
                    "Usage: supercov runs <run-id> decision <id|source-file:line>",
                ));
            };
            request.selector = Some(selector.clone());
        }
        "line" => {
            // Every location named, and every line of a file of them: one
            // call answers them all from one opened run, which is most of
            // what a query costs.
            let mut selectors = options.positional.clone();
            if let Some(path) = &options.from_file {
                let text = supercov_engine::fault_fs::read_to_string(path).map_err(|error| {
                    PublicQueryError::invalid(
                        Some(&agent_command),
                        json_output,
                        format!("Could not read {path}: {error}"),
                    )
                })?;
                selectors.extend(
                    text.lines()
                        .map(str::trim)
                        .filter(|line| !line.is_empty() && !line.starts_with('#'))
                        .map(str::to_owned),
                );
            }
            if selectors.is_empty() {
                return Err(PublicQueryError::invalid(
                    Some(&agent_command),
                    json_output,
                    "Usage: supercov runs <run-id> line <source-file:line>... [--from-file <path>]",
                ));
            }
            let mut locations = selectors
                .iter()
                .map(|selector| {
                    parse_location(selector).ok_or_else(|| {
                        let mut error = PublicQueryError::invalid(
                            Some(&agent_command),
                            json_output,
                            "Expected <source-file>:<line>, with lines counted from 1",
                        );
                        error.error.details = Some(json!({ "selector": selector }));
                        error
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let (file, line) = locations.remove(0);
            request.file = Some(file);
            request.line = Some(line);
            request.more_lines = locations;
        }
        "test" => {
            if options.positional.is_empty() {
                return Err(PublicQueryError::invalid(
                    Some(&agent_command),
                    json_output,
                    "Usage: supercov runs <run-id> test <id|name-fragment>",
                ));
            }
            request.selector = Some(options.positional.join(" ").to_lowercase());
        }
        // `minimize`, the last of the commands the hierarchy lets through.
        _ => {
            request.target = options.target;
            request.max_states = Some(DEFAULT_MAX_STATES);
        }
    }
    Ok((request, options.json))
}

pub fn parse_public_query(
    command: &str,
    arguments: &[String],
) -> Result<PublicQueryInvocation, PublicQueryError> {
    let json_output = arguments.iter().any(|argument| argument == "--json");
    if command == "diff" {
        let options = parse_options(command, arguments, json_output)?;
        let Some(older) = options.positional.first() else {
            return Err(PublicQueryError::invalid(
                Some("diff"),
                json_output,
                "Usage: supercov diff <older-run> <newer-run>",
            ));
        };
        let Some(newer) = options.positional.get(1) else {
            return Err(PublicQueryError::invalid(
                Some("diff"),
                json_output,
                "Usage: supercov diff <older-run> <newer-run>",
            ));
        };
        if let Some(extra) = options.positional.get(2) {
            return Err(PublicQueryError::invalid(
                Some("diff"),
                json_output,
                format!(
                    "unexpected argument {extra}\n\nUsage: supercov diff <older-run> <newer-run>"
                ),
            ));
        }
        return Ok(PublicQueryInvocation::Coverage {
            request: Box::new(IndexedQueryRequest {
                run_id: older.clone(),
                filter: options.filter,
                command: "diff".into(),
                metric: options.metric,
                kind: options.kind,
                runner: options.runner,
                file: None,
                line: None,
                selector: None,
                sort: None,
                valid: None,
                test_exit_code: None,
                stale: None,
                stale_reasons: None,
                offset: options.offset,
                limit: options.limit,
                target: None,
                max_states: None,
                group: None,
                depth: None,
                gained_only: options.gained,
                more_lines: Vec::new(),
                tags: options.tags.clone(),
                titles: options.titles.clone(),
                test_files: options.test_files.clone(),
            }),
            newer_run_id: Some(newer.clone()),
            agent_command: "diff".into(),
            json: options.json,
        });
    }

    debug_assert_eq!(command, "runs");
    let Some(run_id) = arguments
        .first()
        .filter(|argument| !argument.starts_with('-'))
    else {
        let options = parse_options(command, arguments, json_output)?;
        return Ok(PublicQueryInvocation::Runs {
            filter: options.filter,
            offset: options.offset,
            limit: options.limit,
            json: options.json,
        });
    };
    let child_index = 1;
    let child = arguments
        .get(child_index)
        .filter(|child| !child.starts_with('-'))
        .map_or("summary", String::as_str);
    if !matches!(
        child,
        "summary"
            | "kinds"
            | "runners"
            | "scope"
            | "files"
            | "gaps"
            | "tests-without-evidence"
            | "file"
            | "decision"
            | "line"
            | "test"
            | "minimize"
    ) {
        return Err(PublicQueryError::unknown(
            Some(&format!("coverage.{child}")),
            json_output,
            format!("Unknown run query: {child}. Try supercov runs <run-id> --help."),
            json!({ "command": child }),
        ));
    }
    let child_arguments = if arguments
        .get(child_index)
        .is_some_and(|candidate| !candidate.starts_with('-'))
    {
        &arguments[child_index + 1..]
    } else {
        &arguments[child_index..]
    };
    coverage_invocation(run_id.clone(), child, child_arguments, json_output)
}

#[cfg(test)]
mod tests {
    /// Each query's help names its own usage; an unknown one, the list.
    #[test]
    fn each_query_s_help_is_its_own_usage() {
        let help = |child: &str| {
            help_for(
                "runs",
                &["latest".to_owned(), child.to_owned(), "--help".to_owned()],
            )
            .unwrap()
        };
        for child in [
            "summary",
            "files",
            "gaps",
            "tests-without-evidence",
            "kinds",
            "runners",
            "scope",
            "file",
            "decision",
            "line",
            "test",
            "minimize",
        ] {
            let usage = help(child);
            let named = if child == "summary" {
                "[--filter"
            } else {
                child.split('-').next().unwrap()
            };
            assert!(
                usage.starts_with("Usage: supercov runs <run-id> "),
                "{child}: {usage}"
            );
            assert!(usage.contains(named), "{child}: {usage}");
        }
        assert_eq!(help("unknown"), "Usage: supercov runs <run-id> --help\n");
    }

    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn parses_the_instance_first_summary_and_default_pagination() {
        let PublicQueryInvocation::Coverage {
            request,
            agent_command,
            json,
            ..
        } = parse_public_query("runs", &args(&["latest", "--json"])).unwrap()
        else {
            panic!("expected coverage invocation");
        };
        assert_eq!(request.run_id, "latest");
        assert_eq!(request.command, "summary");
        assert_eq!(request.offset, 0);
        assert_eq!(request.limit, 20);
        assert_eq!(agent_command, "coverage.summary");
        assert!(json);

        let PublicQueryInvocation::Coverage { request, .. } =
            parse_public_query("runs", &args(&["run_abc123"])).unwrap()
        else {
            panic!("expected default coverage invocation");
        };
        assert_eq!(request.run_id, "run_abc123");
        assert_eq!(request.command, "summary");
    }

    #[test]
    fn parses_all_structural_file_and_test_options() {
        let PublicQueryInvocation::Coverage { request, .. } = parse_public_query(
            "runs",
            &args(&[
                "run-1",
                "file",
                "src/a.ts",
                "--group",
                "decision",
                "--sort",
                "missing",
                "--metric",
                "mcdc",
                "--filter",
                "passed",
                "--kind",
                "E2E",
                "--runner",
                "Playwright",
                "--offset",
                "40",
            ]),
        )
        .unwrap() else {
            panic!("expected coverage invocation");
        };
        assert_eq!(request.command, "file-decisions");
        assert_eq!(request.file.as_deref(), Some("src/a.ts"));
        assert_eq!(request.sort, Some(DecisionSort::Missing));
        assert_eq!(request.filter, "passed");
        assert_eq!(request.kind.as_deref(), Some("e2e"));
        assert_eq!(request.runner.as_deref(), Some("playwright"));
        assert_eq!(request.offset, 40);
        assert_eq!(request.limit, 20);
    }

    #[test]
    fn splits_line_locations_without_confusing_an_optional_column() {
        let PublicQueryInvocation::Coverage { request, .. } =
            parse_public_query("runs", &args(&["run-1", "line", "src/a.ts:12:8"])).unwrap()
        else {
            panic!("expected coverage invocation");
        };
        assert_eq!(request.file.as_deref(), Some("src/a.ts"));
        assert_eq!(request.line, Some(12));
    }

    #[test]
    fn refuses_to_degrade_a_malformed_instance_query_to_a_listing() {
        let error =
            parse_public_query("runs", &args(&["run-1", "coverage", "--json"])).unwrap_err();
        assert_eq!(error.error.code, ErrorCode::UnknownCommand);
        assert_eq!(error.command.as_deref(), Some("coverage.coverage"));
        assert_eq!(
            error.error.message,
            "Unknown run query: coverage. Try supercov runs <run-id> --help."
        );
        assert!(error.json);
    }

    #[test]
    fn validates_agent_options_before_storage_access() {
        let error =
            parse_public_query("runs", &args(&["run-1", "gaps", "--limit", "-1", "--json"]))
                .unwrap_err();
        assert_eq!(error.error.code, ErrorCode::InvalidArgument);
        assert_eq!(error.command.as_deref(), Some("coverage.gaps"));
        assert_eq!(
            error.error.message,
            "--limit must be a number of entries, or 0 for all of them"
        );
    }

    /// Each option missing what it needs, or given where it means nothing,
    /// is refused with what it needs.
    #[test]
    fn an_option_missing_its_value_or_out_of_place_says_what_it_needs() {
        for (arguments, message) in [
            (&["run-1", "--run"][..], "--run requires a run ID"),
            (&["run-1", "--kind"], "--kind requires a test kind"),
            (&["run-1", "--runner"], "--runner requires a runner name"),
            (&["run-1", "--tag", ""], "--tag requires a tag"),
            (&["run-1", "--title"], "--title requires text"),
            (&["run-1", "--test-file", ""], "--test-file requires"),
            (
                &["run-1", "files", "--files"],
                "--files is only supported by",
            ),
            (
                &["run-1", "line", "--from-file"],
                "--from-file needs a file",
            ),
            (
                &["run-1", "file", "a.rs", "--group", "dir"],
                "--group must be decision",
            ),
            (&["run-1", "files", "--group"], "--group must be dir"),
            (
                &["run-1", "gaps", "--group", "dir"],
                "--group is only supported by",
            ),
            (
                &["run-1", "file"],
                "Usage: supercov runs <run-id> file <source-file>",
            ),
            (
                &["run-1", "line", "a.rs:x"],
                "Expected <source-file>:<line>",
            ),
            (
                &["run-1", "line", "--from-file", "/nonexistent/locations"],
                "Could not read /nonexistent/locations",
            ),
        ] {
            let error = parse_public_query("runs", &args(arguments)).unwrap_err();
            assert_eq!(
                error.error.code,
                ErrorCode::InvalidArgument,
                "{arguments:?}"
            );
            assert!(
                error.error.message.contains(message),
                "{arguments:?}: {}",
                error.error.message
            );
        }
        let error = parse_public_query("diff", &args(&["run-1"])).unwrap_err();
        assert_eq!(
            error.error.message,
            "Usage: supercov diff <older-run> <newer-run>"
        );
        let error = parse_public_query("diff", &args(&["run-1", "--kind"])).unwrap_err();
        assert_eq!(error.error.message, "--kind requires a test kind");
        let PublicQueryInvocation::Coverage { request, .. } =
            parse_public_query("runs", &args(&["run-1", "minimize", "--target", "90"])).unwrap()
        else {
            panic!("expected a minimize query");
        };
        assert_eq!(request.command, "minimize");
        assert_eq!(request.max_states, Some(DEFAULT_MAX_STATES));
    }
}
