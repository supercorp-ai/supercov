//! Advisory, source-only quality assessment. No coverage run or test execution.
//!
//! One instrument: the named-property catalog in `smells.json`. The nine-construct
//! rubric this module used to carry was removed on 2026-09-18, because it ordered
//! human-rated classes 90.2% correctly where counting the statements in a file
//! got 90.3%, and a paid call cannot justify itself by matching `wc`. Declaration
//! scope went with it: graded declarations located the function a maintainer
//! fixed 5 times in 12 against 3.77 by chance, and reformatting a file moved two
//! of its constructs more than real fixes did.
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

mod changes;
mod query;
mod scope;
mod smells;
mod store;

const MODEL: &str = "jev-1.13.0";
const ENDPOINT: &str = "https://api.typesafe.ai/v1/systemone";
// Jev 1.13.0 documents 32k tokens for state plus the longest question and 64k
// for state plus every question. This budget targets the tighter limit and
// leaves room for the estimate below to be wrong. Source is never truncated;
// a file over budget is assessed as windows instead.
const MAX_REQUEST_TOKENS: usize = 30_000;
// Bytes per token, deliberately low. A 105,562-byte Rust file measured 3.8
// bytes per token on jev-1.13.0; dense, minified or non-Latin source packs more
// tokens into the same bytes, so estimating low keeps us inside the limit.
const BYTES_PER_TOKEN: usize = 3;
// Nothing larger is read into memory, whether it would be windowed or not.
const MAX_SOURCE_BYTES: usize = 4 << 20;
// A declaration larger than half the budget is split at its own children rather
// than kept whole as a window boundary. Without this a file holding a single
// large class has one boundary, so it cannot be split at all, and most Java
// files hold a single class.
const SPLIT_INSIDE_BYTES: usize = MAX_REQUEST_TOKENS * BYTES_PER_TOKEN / 2;
const MAX_RESPONSE_BYTES: u64 = 1_048_576;
// Shared by every construct that does not supply wording of its own.
// Used in place of GRADING_TASK when only part of a file is supplied.
// The largest movement seen between two identical requests when this rubric
// family was measured on real files: 28 source snapshots across 8 constructs,
// where the median difference was 0.075 and the largest 0.700. A movement no
// larger than this is reported with that fact attached rather than hidden. It
// is an observed maximum, not a statistical threshold, and an identical request
// is answered from cache anyway, so an unchanged file usually moves by zero.
// Weakest maintainability first, then readability, then Jev's own overall answer.
// These are the two constructs with calibrated cutoffs and human references.

const HELP: &str = "Assess source quality with TypeSafe AI (experimental, advisory).

Usage:
  supercov quality [file-or-directory ...]     assess code, saving a snapshot
  supercov quality gaps [snapshot]             only files something fired on
  supercov quality file <path> [snapshot]      one file, every check
  supercov quality scope                       which files are assessed, and why
  supercov quality snapshots                   list saved assessments
  supercov quality show [snapshot]             read a saved assessment
  supercov quality diff <snapshot> <snapshot>  what declined between two
  supercov quality patch [file-or-directory]   what a change introduced

Change range (quality patch), pick one:
  --unstaged          Working tree against the index (default)
  --staged            Index against HEAD: what a commit would contain
  --base <ref>        Working tree against the merge base with a branch or tag

Patch options:
  --annotate github   Print GitHub workflow annotations; posts nothing

Assessment options:
  --all               Include test files, generated output and anything outside
                      a source root, all of which are left out by default
  --json              Print the report as JSON
  --dry-run           Print exact request bodies as JSON; no API call, no writes
  --refresh           Bypass cached responses

Reading options:
  --json              Print the view as JSON
  --limit <n>         Rows to show, 0 for all of them (default 20)

Twelve named properties are asked of each file as yes/no questions, and the
arithmetic that turns twelve answers into one number is done here rather than by
the model, so every part of a score is a claim you can check against the file.
Text reports a band because the measured resolution is about a point; --json
carries the number.

With no path the subject is this repository. Which files that means is decided
the way coverage decides it: source roots found from package manifests, with
test code and generated output left out, and anything under no recognised root
reported rather than guessed at. SUPERCOV_SOURCE_ROOTS=src,app declares them.

Assessments are advisory. No finding fails this command.

Cache: .supercov/quality/requests/ (exact request hash; --refresh to reassess).
Snapshots: .supercov/quality/snapshots/.
";

#[derive(Default, Debug)]
struct Options {
    paths: Vec<PathBuf>,
    /// Assess test files and generated output too, which discovery leaves out.
    all: bool,
    json: bool,
    dry_run: bool,
    refresh: bool,
    context: Option<PathBuf>,
    review_below: Option<f64>,
}

/// What the user asked for: a new assessment, or a reading of a saved one.
/// Reading never contacts the provider.
enum Command {
    Snapshots {
        json: bool,
        limit: usize,
    },
    /// Which files an assessment is about, and why, mirroring `runs scope`.
    Scope {
        json: bool,
        limit: usize,
    },
    /// Only the files something fired on, mirroring `runs gaps`.
    Gaps {
        snapshot: Option<String>,
        json: bool,
        limit: usize,
    },
    Show {
        snapshot: Option<String>,
        json: bool,
        limit: usize,
    },
    File {
        path: String,
        snapshot: Option<String>,
        json: bool,
    },
    Diff {
        from: String,
        to: String,
        json: bool,
        limit: usize,
    },
    /// The catalog: twelve named properties, composed here. The default.
    Health(Options),
    /// Ask the same catalog what a change introduced.
    Patch {
        range: changes::Range,
        paths: Vec<PathBuf>,
        json: bool,
        refresh: bool,
        limit: usize,
        annotate: bool,
        all: bool,
    },
}

/// The change range, shared by any command that works on a change rather than
/// on a tree. Kept separate from scan's options so a later command can take the
/// same three flags without inheriting the rest.
fn parse_patch(arguments: Vec<String>) -> Result<Command, String> {
    let (mut range, mut json, mut refresh, mut limit) = (None, false, false, None);
    let mut annotate = false;
    let mut all = false;
    let mut paths = Vec::new();
    let mut arguments = arguments.into_iter();
    let set = |chosen: changes::Range, slot: &mut Option<changes::Range>| match slot {
        Some(existing) if *existing != chosen => {
            Err("pick one change range: --unstaged, --staged or --since <ref>".to_string())
        }
        slot => {
            *slot = Some(chosen);
            Ok(())
        }
    };
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--json" => json = true,
            "--refresh" => refresh = true,
            "--all" => all = true,
            "--unstaged" => set(changes::Range::Unstaged, &mut range)?,
            "--staged" | "--cached" => set(changes::Range::Staged, &mut range)?,
            "--base" => {
                let reference = arguments
                    .next()
                    .ok_or("--base needs a commit, branch or tag")?;
                set(changes::Range::Base(reference), &mut range)?;
            }
            "--annotate" => {
                // Only GitHub's form exists, and naming it keeps room for others.
                match arguments.next().as_deref() {
                    Some("github") => annotate = true,
                    Some(other) => {
                        return Err(format!("--annotate does not know {other}; use github"));
                    }
                    None => return Err("--annotate needs a format; use github".into()),
                }
            }
            "--limit" => {
                limit = Some(
                    arguments
                        .next()
                        .ok_or("--limit needs a number")?
                        .parse::<usize>()
                        .map_err(|_| "--limit needs a number")?,
                );
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other} for quality patch"));
            }
            other => paths.push(PathBuf::from(other)),
        }
    }
    Ok(Command::Patch {
        range: range.unwrap_or(changes::Range::Unstaged),
        paths,
        json,
        refresh,
        limit: limit.unwrap_or(20),
        annotate,
        all,
    })
}

/// The first argument is a subcommand only when it is one of the reserved
/// words. A directory that happens to be called `show` is still assessable as
/// `supercov quality scan show`.
fn parse(arguments: Vec<String>) -> Result<Command, String> {
    let subcommand = arguments.first().map_or("", String::as_str);
    let rest = || arguments[1..].to_vec();
    match subcommand {
        "scan" => Ok(Command::Health(defaulted(parse_scan(rest())?))),
        "snapshots" | "show" | "gaps" | "scope" | "file" | "diff" => parse_view(subcommand, rest()),
        "patch" => parse_patch(rest()),
        _ => Ok(Command::Health(defaulted(parse_scan(arguments)?))),
    }
}

fn parse_view(kind: &str, arguments: Vec<String>) -> Result<Command, String> {
    let mut json = false;
    let mut limit = None;
    let mut positional = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--json" => json = true,
            "--limit" => {
                let value = arguments
                    .next()
                    .and_then(|value| value.parse::<usize>().ok())
                    .ok_or("--limit requires a row count, or 0 for every row")?;
                if limit.replace(value).is_some() {
                    return Err("--limit may only be specified once".into());
                }
            }
            "--" => positional.extend(arguments.by_ref()),
            value if value.starts_with('-') => {
                return Err(format!("unknown quality {kind} option: {value}"));
            }
            _ => positional.push(argument),
        }
    }
    if kind == "file" && limit.is_some() {
        return Err(format!(
            "quality {kind} shows one thing and takes no --limit"
        ));
    }
    let limit = limit.unwrap_or(query::DEFAULT_LIMIT);
    let mut positional = positional.into_iter();
    let mut required = |what: &str| {
        positional
            .next()
            .ok_or_else(|| format!("quality {kind} requires {what}"))
    };
    Ok(match kind {
        "snapshots" => Command::Snapshots { json, limit },
        "scope" => Command::Scope { json, limit },
        "gaps" => Command::Gaps {
            snapshot: positional.next(),
            json,
            limit,
        },
        "show" => Command::Show {
            snapshot: positional.next(),
            json,
            limit,
        },
        "file" => Command::File {
            path: required("a file path")?,
            snapshot: positional.next(),
            json,
        },
        "diff" => Command::Diff {
            from: required("two snapshots to compare")?,
            to: required("a second snapshot to compare")?,
            json,
            limit,
        },
        other => return Err(format!("unknown quality view: {other}")),
    })
}

/// With no path, the subject is everything here. `supercov quality` should say
/// something about this repository without being told where to look, the way
/// `supercov runs` needs no argument.
fn here() -> Vec<PathBuf> {
    vec![PathBuf::from(".")]
}

fn defaulted(mut options: Options) -> Options {
    if options.paths.is_empty() {
        options.paths = here();
    }
    options
}

fn parse_scan(args: Vec<String>) -> Result<Options, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--json" => options.json = true,
            "--dry-run" => options.dry_run = true,
            "--refresh" => options.refresh = true,
            "--all" => options.all = true,
            "--review-below" => {
                let value = args
                    .next()
                    .ok_or("--review-below requires a number from 0 to 10")?;
                let value: f64 = value
                    .parse()
                    .map_err(|_| "--review-below requires a number from 0 to 10")?;
                if !value.is_finite() || !(0.0..=10.0).contains(&value) {
                    return Err("--review-below requires a number from 0 to 10".into());
                }
                if options.review_below.replace(value).is_some() {
                    return Err("--review-below may only be specified once".into());
                }
            }
            "--context" => {
                let path = args
                    .next()
                    .filter(|s| !s.starts_with('-'))
                    .ok_or("--context requires a file")?;
                if options.context.replace(path.into()).is_some() {
                    return Err("--context may only be specified once".into());
                }
            }
            "--" => {
                options.paths.extend(args.map(PathBuf::from));
                break;
            }
            value if value.starts_with('-') => {
                return Err(format!("unknown quality option: {value}"));
            }
            path => options.paths.push(path.into()),
        }
    }
    // No path is not an error: the caller fills in this directory. Asking for
    // the quality of the repository you are standing in should need no argument.
    Ok(options)
}

/// A contiguous group of whole top-level declarations, in one-based inclusive
/// lines. The windows of a file partition it: every line belongs to exactly one.
#[derive(Debug, Clone, Serialize)]
struct Window {
    index: usize,
    of: usize,
    start_line: usize,
    end_line: usize,
    declarations: Vec<String>,
}

/// Tokens the provider is likely to charge for a serialized request. An
/// estimate, never a measurement; `BYTES_PER_TOKEN` explains the direction.
fn estimated_tokens(bytes: usize) -> usize {
    bytes.div_ceil(BYTES_PER_TOKEN)
}

/// The serialized request, when it fits the token budget.
fn within_budget(request: &Value) -> Result<Option<Vec<u8>>, String> {
    let bytes = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    Ok((estimated_tokens(bytes.len()) <= MAX_REQUEST_TOKENS).then_some(bytes))
}

/// Byte offset of every line start, so a window can be sliced without
/// rewriting a single byte. Line endings stay exactly as the file has them.
fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .filter(|start| *start < source.len())
        .collect()
}

fn window_source<'s>(source: &'s str, starts: &[usize], window: &Window) -> &'s str {
    let from = starts[window.start_line - 1];
    let to = starts.get(window.end_line).copied().unwrap_or(source.len());
    &source[from..to]
}

/// The declarations a file is split around: its top-level ones, descending into
/// any declaration too large to send on its own. A file that is a single class
/// splits at that class's methods, rather than having one boundary and so no
/// way to be split at all. `None` when Supercov has no parser for the language
/// or the file does not parse.
fn split_units(path: &str, source: &str) -> Option<(Value, Vec<(String, usize)>)> {
    let code = supercov_engine::source_units::code(path, source)?;
    let lines = line_starts(source);
    let offset = |line: usize, column: usize| {
        lines
            .get(line - 1)
            .map_or(source.len(), |start| start + column - 1)
    };
    let children = |parent: usize| -> Vec<usize> {
        code.units
            .iter()
            .enumerate()
            .filter(|(_, unit)| unit.parent == Some(parent))
            .map(|(index, _)| index)
            .collect()
    };
    fn collect(
        parent: usize,
        children: &impl Fn(usize) -> Vec<usize>,
        span: &impl Fn(usize) -> usize,
        found: &mut Vec<usize>,
    ) {
        for index in children(parent) {
            let inner = children(index);
            if span(index) > SPLIT_INSIDE_BYTES && !inner.is_empty() {
                collect(index, children, span, found);
            } else {
                found.push(index);
            }
        }
    }
    let span = |index: usize| {
        let unit = &code.units[index];
        offset(unit.end_line, unit.end_column).saturating_sub(offset(unit.line, unit.column))
    };
    let mut found = Vec::new();
    collect(0, &children, &span, &mut found);
    let units: Vec<&supercov_engine::source_units::Unit> =
        found.iter().map(|index| &code.units[*index]).collect();
    let outline = Value::Array(
        units
            .iter()
            .map(|unit| {
                json!({"name": unit.path, "kind": unit.kind, "start_line": unit.line, "end_line": unit.end_line})
            })
            .collect(),
    );
    Some((
        outline,
        units
            .iter()
            .map(|unit| (unit.path.clone(), unit.line))
            .collect(),
    ))
}

/// Every line of the file assigned to one candidate window, split only at the
/// first line of a top-level declaration. Code before the first declaration
/// joins it rather than becoming a window of its own.
fn candidates(path: &str, source: &str, lines: &[usize]) -> Option<(Value, Vec<Window>)> {
    let (outline, top) = split_units(path, source)?;
    if top.is_empty() {
        return None;
    }
    let mut boundaries: Vec<usize> = top.iter().map(|(_, line)| *line).collect();
    boundaries.dedup();
    let windows = boundaries
        .iter()
        .enumerate()
        .map(|(index, boundary)| {
            let start_line = if index == 0 { 1 } else { *boundary };
            let end_line = boundaries
                .get(index + 1)
                .map_or(lines.len(), |next| next - 1);
            Window {
                index: index + 1,
                of: boundaries.len(),
                start_line,
                end_line,
                declarations: top
                    .iter()
                    .filter(|(_, line)| *line >= start_line && *line <= end_line)
                    .map(|(name, _)| name.clone())
                    .collect(),
            }
        })
        .collect();
    Some((outline, windows))
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn is_source(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|s| s.to_str()),
        Some(
            "js" | "jsx"
                | "mjs"
                | "cjs"
                | "ts"
                | "tsx"
                | "mts"
                | "cts"
                | "rs"
                | "py"
                | "pyi"
                | "rb"
                | "go"
                | "java"
                | "kt"
                | "kts"
                | "c"
                | "h"
                | "cc"
                | "cpp"
                | "hpp"
                | "cs"
                | "swift"
                | "php"
        )
    )
}

/// Why a file is left out of an assessment by default.
///
/// Not a judgment that the code does not matter. A score is about the code you
/// ship, these files cost a request each, and several checks encode assumptions
/// that are wrong for them: duplication between two test cases is often
/// deliberate and good, and a literal in a fixture is the fixture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Skipped {
    Test,
    Generated,
}

impl Skipped {
    fn reason(self) -> &'static str {
        match self {
            Skipped::Test => "test",
            Skipped::Generated => "generated",
        }
    }
}

/// Directory names that mean test code in every language this assesses.
const TEST_DIRECTORIES: &[&str] = &[
    "__mocks__",
    "__snapshots__",
    "__tests__",
    "cypress",
    "e2e",
    "features",
    "fixture",
    "fixtures",
    "mock",
    "mocks",
    "spec",
    "specs",
    "test",
    "testdata",
    "tests",
];

/// A CamelCase suffix that names a test class, for the languages that spell it
/// that way rather than with a separator: Java, Kotlin, C#, Swift, PHP.
fn camel_test_suffix(stem: &str) -> bool {
    for suffix in ["Test", "Tests", "TestCase", "TestCases", "Spec", "Specs"] {
        if stem.len() > suffix.len() && stem.ends_with(suffix) {
            return true;
        }
    }
    // `OrderIT` is an integration test; `AUDIT` is not. Requiring a lowercase
    // letter before the suffix separates them.
    stem.len() > 2
        && stem.ends_with("IT")
        && stem
            .chars()
            .nth(stem.len() - 3)
            .is_some_and(|c| c.is_ascii_lowercase())
}

/// Whether a path is test code or generated output, and which.
///
/// Matching is on whole separator-delimited parts, never on substrings, so
/// `latest.ts`, `contest.rs`, `manifest.java` and `attestation.go` are source.
fn skipped_path(relative: &str) -> Option<Skipped> {
    let lower = relative.to_ascii_lowercase();
    let (directories, file) = match lower.rsplit_once('/') {
        Some((directories, file)) => (directories, file),
        None => ("", lower.as_str()),
    };
    if directories
        .split('/')
        .any(|segment| TEST_DIRECTORIES.contains(&segment))
    {
        return Some(Skipped::Test);
    }
    let parts: Vec<&str> = file.split(['.', '_', '-']).collect();
    if parts.iter().any(|part| matches!(*part, "test" | "spec")) {
        return Some(Skipped::Test);
    }
    if matches!(
        file,
        "conftest.py" | "spec_helper.rb" | "test_helper.rb" | "rails_helper.rb"
    ) {
        return Some(Skipped::Test);
    }
    let stem = relative.rsplit('/').next().unwrap_or(relative);
    let stem = stem.split('.').next().unwrap_or(stem);
    if camel_test_suffix(stem) {
        return Some(Skipped::Test);
    }
    // Type declarations carry no logic, and the rest are the file names that
    // code generators produce across these ecosystems.
    if lower.ends_with(".d.ts") || lower.ends_with(".d.mts") || lower.ends_with(".d.cts") {
        return Some(Skipped::Generated);
    }
    if lower.ends_with(".min.js")
        || lower.ends_with(".pb.go")
        || lower.ends_with(".pb.cc")
        || lower.ends_with(".pb.h")
        || lower.ends_with("_pb2.py")
        || lower.ends_with("_pb2_grpc.py")
        || lower.ends_with(".designer.cs")
        || lower.ends_with(".g.cs")
        || parts.contains(&"generated")
    {
        return Some(Skipped::Generated);
    }
    None
}

/// The marker a generator leaves in the file itself, which is the one signal
/// that does not depend on a naming convention. Go's is a standard; the others
/// are conventions wide enough to be worth reading.
fn generated_header(source: &str) -> bool {
    source
        .char_indices()
        .take_while(|(index, _)| *index < 2048)
        .last()
        .map(|(index, c)| &source[..index + c.len_utf8()])
        .unwrap_or(source)
        .lines()
        .take(20)
        .any(|line| {
            line.contains("Code generated by")
                || line.contains("@generated")
                || line.contains("DO NOT EDIT")
                || line.contains("do not edit")
                || line.contains("auto-generated")
                || line.contains("autogenerated")
        })
}

fn ignored_directory(name: &str) -> bool {
    matches!(
        name,
        "node_modules"
            | "target"
            | "vendor"
            | "dist"
            | "build"
            | "coverage"
            | "generated"
            | "__pycache__"
            | "venv"
            | "out"
    )
}

fn discover(root: &Path, paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = BTreeSet::new();
    for path in paths {
        let input = root.join(path);
        let metadata =
            fs::symlink_metadata(&input).map_err(|e| format!("{}: {e}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "symbolic link is not a quality target: {}",
                path.display()
            ));
        }
        let canonical = input.canonicalize().map_err(|e| e.to_string())?;
        if !canonical.starts_with(root) {
            return Err(format!(
                "quality target is outside current directory: {}",
                path.display()
            ));
        }
        for ancestor in input.ancestors().take_while(|ancestor| *ancestor != root) {
            if fs::symlink_metadata(ancestor)
                .map_err(|e| e.to_string())?
                .file_type()
                .is_symlink()
            {
                return Err(format!(
                    "symbolic link in quality target path: {}",
                    path.display()
                ));
            }
        }
        if metadata.is_file() {
            if !is_source(&canonical) {
                return Err(format!("unsupported source extension: {}", path.display()));
            }
            files.insert(canonical);
        } else if metadata.is_dir() {
            let walker = ignore::WalkBuilder::new(&canonical)
                .follow_links(false)
                .require_git(false)
                .filter_entry(|entry| {
                    entry.depth() == 0
                        || !entry.file_type().is_some_and(|t| t.is_dir())
                        || !ignored_directory(&entry.file_name().to_string_lossy())
                })
                .build();
            for entry in walker {
                let entry = entry.map_err(|e| e.to_string())?;
                if entry.file_type().is_some_and(|t| t.is_file()) && is_source(entry.path()) {
                    files.insert(entry.into_path());
                }
            }
        } else {
            return Err(format!(
                "not a regular file or directory: {}",
                path.display()
            ));
        }
    }
    if files.is_empty() {
        return Err("no supported source files found".into());
    }
    Ok(files.into_iter().collect())
}

fn read_text(path: &Path) -> Result<String, String> {
    let file = fs::File::open(path).map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_SOURCE_BYTES {
        return Err(format!(
            "source/context is larger than {MAX_SOURCE_BYTES} bytes and is not assessed"
        ));
    }
    String::from_utf8(bytes).map_err(|_| "source/context must be valid UTF-8".into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ApiResponse {
    model: String,
    answers: BTreeMap<String, Answer>,
    usage: Usage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Answer {
    Noul {
        noul: f64,
    },
    Choice {
        choice: String,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
    Score {
        score: f64,
        legend: BTreeMap<String, String>,
        probabilities: BTreeMap<String, f64>,
        confidence: f64,
    },
}

fn probability(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn distribution(values: &BTreeMap<String, f64>, keys: BTreeSet<String>) -> bool {
    // Live responses round each probability to two decimal places separately.
    // Their serialized sum can therefore differ from one by up to n * 0.005.
    let rounding_error = values.len() as f64 * 0.005 + 1e-9;
    values.keys().cloned().collect::<BTreeSet<_>>() == keys
        && values.values().all(|&v| probability(v))
        && (values.values().sum::<f64>() - 1.0).abs() <= rounding_error
}

fn validate(response: &ApiResponse, request: &Value) -> Result<(), String> {
    if response.model != MODEL {
        return Err(format!(
            "expected model {MODEL}, received {}",
            response.model
        ));
    }
    let questions = request["questions"]
        .as_object()
        .ok_or("invalid request questions")?;
    if response.answers.keys().collect::<BTreeSet<_>>() != questions.keys().collect::<BTreeSet<_>>()
    {
        return Err("response question IDs do not match request".into());
    }
    for (id, question) in questions {
        let valid = match &response.answers[id] {
            Answer::Noul { noul } => question["type"] == "noul" && probability(*noul),
            Answer::Choice {
                choice,
                probabilities,
                confidence,
            } => {
                let options = question["criteria"]
                    .as_object()
                    .filter(|_| question["type"] == "choice");
                options.is_some_and(|options| {
                    let keys: BTreeSet<String> = options.keys().cloned().collect();
                    keys.contains(choice)
                        && probability(*confidence)
                        && distribution(probabilities, keys)
                })
            }
            Answer::Score {
                score,
                legend,
                probabilities,
                confidence,
            } => {
                let levels = question["criteria"]
                    .as_array()
                    .filter(|_| question["type"] == "score");
                levels.is_some_and(|levels| {
                    let keys = (0..levels.len()).map(|i| i.to_string()).collect();
                    let expected: f64 = (0..levels.len())
                        .map(|i| {
                            i as f64 * probabilities.get(&i.to_string()).copied().unwrap_or(0.0)
                        })
                        .sum();
                    // The provider rounds the score and each probability
                    // independently. Bound both errors, including float noise.
                    let rounding_error =
                        0.005 * (1 + (0..levels.len()).sum::<usize>()) as f64 + 1e-9;
                    score.is_finite()
                        && *score >= 0.0
                        && *score <= (levels.len() - 1) as f64
                        && probability(*confidence)
                        && distribution(probabilities, keys)
                        && (*score - expected).abs() <= rounding_error
                        && legend.len() == levels.len()
                        && levels.iter().enumerate().all(|(i, level)| {
                            legend.get(&i.to_string()).map(String::as_str) == level.as_str()
                        })
                })
            }
        };
        if !valid {
            return Err(format!("invalid TypeSafe answer for {id}"));
        }
    }
    Ok(())
}

/// A validated Noul answer, or `None` when this question set did not ask it.
fn noul(response: &ApiResponse, id: &str) -> Option<f64> {
    match response.answers.get(id) {
        Some(Answer::Noul { noul }) => Some(*noul),
        _ => None,
    }
}

fn client() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(45)))
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn evaluate(
    agent: &ureq::Agent,
    endpoint: &str,
    key: &str,
    request: &Value,
) -> Result<ApiResponse, String> {
    for attempt in 0..3 {
        let mut response = agent
            .post(endpoint)
            .header("Authorization", format!("Bearer {key}"))
            .send_json(request)
            .map_err(|_| {
                "TypeSafe transport failed (connection, TLS or timeout); retry the command"
                    .to_string()
            })?;
        let status = response.status().as_u16();
        if (status == 429 || (500..600).contains(&status)) && attempt < 2 {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|s| s.to_str().ok())
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(1 << attempt);
            if delay > 10 {
                return Err(format!(
                    "TypeSafe HTTP {status}; retry after {delay} seconds"
                ));
            }
            std::thread::sleep(Duration::from_secs(delay));
            continue;
        }
        if !(200..300).contains(&status) {
            return Err(format!(
                "TypeSafe HTTP {status} (check credentials, quota or request size)"
            ));
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_RESPONSE_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "failed to read TypeSafe response".to_string())?;
        if bytes.len() as u64 > MAX_RESPONSE_BYTES {
            return Err("TypeSafe response exceeds size limit".into());
        }
        let parsed: ApiResponse = serde_json::from_slice(&bytes)
            .map_err(|_| "TypeSafe returned an invalid response schema".to_string())?;
        validate(&parsed, request)?;
        return Ok(parsed);
    }
    unreachable!("last attempt returns")
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    request_hash: String,
    response: ApiResponse,
    elapsed_ms: u64,
}

fn cached(path: &Path, hash: &str, request: &Value) -> Result<Option<CacheEntry>, String> {
    match fs::read(path) {
        Ok(bytes) => {
            let entry: CacheEntry = serde_json::from_slice(&bytes)
                .map_err(|_| "invalid quality cache; use --refresh")?;
            if entry.request_hash != hash {
                return Err("quality cache hash mismatch; use --refresh".into());
            }
            validate(&entry.response, request)
                .map_err(|e| format!("{e} in cache; use --refresh"))?;
            Ok(Some(entry))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("cannot read quality cache: {e}")),
    }
}

fn save(path: &Path, entry: &CacheEntry) -> Result<(), String> {
    store::write_atomic(path, &serde_json::to_vec(entry).map_err(|e| e.to_string())?)
}

/// One request answered: from the cache when this exact request was answered
/// before, otherwise from the provider.
fn resolve(
    project_root: &Path,
    agent: &ureq::Agent,
    key: Option<&str>,
    refresh: bool,
    request: &Value,
    hash: &str,
) -> Result<(CacheEntry, bool, Option<String>), String> {
    let cache_path = store::responses(project_root).join(format!("{hash}.json"));
    if !refresh {
        // The flat directory is where responses were cached before snapshots
        // existed. An answer saved there still answers this exact request.
        for path in [&cache_path, &store::legacy_response(project_root, hash)] {
            if let Some(entry) = cached(path, hash, request)? {
                return Ok((entry, true, None));
            }
        }
    }
    let key = key
        .filter(|s| !s.trim().is_empty())
        .ok_or("set TYPESAFE_API_KEY for uncached assessments, or use --dry-run")?;
    let started = Instant::now();
    let response = evaluate(agent, ENDPOINT, key, request)?;
    let entry = CacheEntry {
        request_hash: hash.to_owned(),
        response,
        elapsed_ms: started.elapsed().as_millis() as u64,
    };
    let warning = save(&cache_path, &entry)
        .err()
        .map(|e| format!("assessment completed but cache could not be saved: {e}"));
    Ok((entry, false, warning))
}

/// How many requests are in flight at once. A scan spends nearly all its time
/// waiting on the provider, which allows 1,200 requests a minute, so a handful
/// in flight turns minutes into seconds while staying well inside that. Each
/// answer is saved under its own request hash, so no two threads ever write the
/// same file.
const CONCURRENCY: usize = 8;

/// A request answered: its hash, the answer, whether it came from the cache,
/// and any trouble saving it.
type Answered = Result<(String, CacheEntry, bool, Option<String>), String>;

/// Which request an answer belongs to: a file, and a window within it when the
/// file was too large to send whole.
type Slot = (usize, Option<usize>);

/// Answer every prepared request, a few at a time.
fn answer_all(
    root: &Path,
    agent: &ureq::Agent,
    key: Option<&str>,
    refresh: bool,
    pending: &[(Slot, Value, Vec<u8>)],
    progress: bool,
) -> BTreeMap<Slot, Answered> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let finished = AtomicUsize::new(0);
    let answers = std::sync::Mutex::new(BTreeMap::new());
    std::thread::scope(|scope| {
        for _ in 0..CONCURRENCY.min(pending.len()) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((slot, request, bytes)) = pending.get(index) else {
                        return;
                    };
                    let hash = digest(bytes);
                    let answered = resolve(root, agent, key, refresh, request, &hash)
                        .map(|(entry, hit, warning)| (hash, entry, hit, warning));
                    if progress {
                        let count = finished.fetch_add(1, Ordering::Relaxed) + 1;
                        eprintln!("[supercov] quality: {count}/{}", pending.len());
                    }
                    answers
                        .lock()
                        .expect("answers are only locked to insert one")
                        .insert(*slot, answered);
                }
            });
        }
    });
    answers
        .into_inner()
        .expect("answers are only locked to insert one")
}

/// A saved view: JSON for a program, text for a person. Reading never fails
/// the command on the strength of what it found.
/// Windows for a file too large to send whole, planned for the catalog.
///
/// A single file can be split where a pair of versions cannot: there is only
/// one text, so a window is still a real piece of code to ask about. Windows
/// merge greedily while the merged request still fits, which keeps their number
/// down and each one as wide as possible.
fn catalog_windows(path: &str, source: &str) -> Result<Vec<(Window, String)>, String> {
    let starts = line_starts(source);
    let (_, candidates) = candidates(path, source, &starts).ok_or(
        "file is over the request budget and has no parsed top-level declarations to window on",
    )?;
    let fits = |window: &Window| -> Result<bool, String> {
        let text = window_source(source, &starts, window);
        Ok(within_budget(&smells::file_request(path, text))?.is_some())
    };
    let mut planned: Vec<Window> = Vec::new();
    for candidate in candidates {
        let merged = planned.last().map(|open| Window {
            index: open.index,
            of: open.of,
            start_line: open.start_line,
            end_line: candidate.end_line,
            declarations: [open.declarations.clone(), candidate.declarations.clone()].concat(),
        });
        match merged {
            Some(merged) if fits(&merged)? => {
                *planned.last_mut().expect("a window to merge into") = merged;
            }
            _ => planned.push(candidate),
        }
    }
    let total = planned.len();
    Ok(planned
        .into_iter()
        .enumerate()
        .map(|(index, mut window)| {
            window.index = index + 1;
            window.of = total;
            let text = window_source(source, &starts, &window).to_owned();
            (window, text)
        })
        .collect())
}

/// One subject to ask the smell catalog about: a file as it stands, or a file
/// before and after a change. Both produce the same twelve answers, so both
/// travel through one sender.
struct Subject {
    path: String,
    bytes: u64,
    request: Value,
    /// Set when this subject is not being asked the preferred question, so the
    /// report can say the answer is weaker rather than look the same.
    note: Option<String>,
}

/// Every answer for one subject, or why it has none.
struct Answers {
    path: String,
    bytes: u64,
    /// How many requests this row came from. More than one means the file was
    /// windowed, which the report says.
    windows: usize,
    note: Option<String>,
    values: Option<BTreeMap<String, f64>>,
    cached: bool,
    error: Option<String>,
}

/// Send every subject, a few at a time, reusing the scan cache and retry path.
fn ask_smells(
    root: &Path,
    key: Option<&str>,
    refresh: bool,
    subjects: Vec<Subject>,
    progress: bool,
) -> (Vec<Answers>, Usage) {
    let agent = client();
    let mut pending = Vec::new();
    let mut out: Vec<Answers> = Vec::new();
    for (index, subject) in subjects.iter().enumerate() {
        let sendable = within_budget(&subject.request);
        let error = match &sendable {
            Ok(Some(_)) => None,
            Ok(None) => Some(format!(
                "over the {MAX_REQUEST_TOKENS} token request budget; a pair of versions \
                 cannot be split into windows the way one file can"
            )),
            Err(e) => Some(e.clone()),
        };
        if let Ok(Some(bytes)) = sendable {
            pending.push(((index, None), subject.request.clone(), bytes));
        }
        out.push(Answers {
            path: subject.path.clone(),
            bytes: subject.bytes,
            windows: 1,
            note: subject.note.clone(),
            values: None,
            cached: false,
            error,
        });
    }
    let answered = answer_all(root, &agent, key, refresh, &pending, progress);
    let mut usage = Usage {
        input_tokens: 0,
        output_tokens: 0,
    };
    for ((index, _), result) in answered {
        match result {
            Ok((_, entry, hit, warning)) => {
                usage.input_tokens += entry.response.usage.input_tokens;
                usage.output_tokens += entry.response.usage.output_tokens;
                out[index].values = Some(
                    smells::catalog()
                        .iter()
                        .filter_map(|c| Some((c.id.clone(), noul(&entry.response, &c.id)?)))
                        .collect(),
                );
                out[index].cached = hit;
                out[index].error = warning;
            }
            Err(e) => out[index].error = Some(e),
        }
    }
    (out, usage)
}

/// Fold a windowed file's answers back into one.
///
/// The highest value wins for each check, because every question is existential:
/// "does this file contain a method that..." is true if any window has one. The
/// note on a windowed file says which properties that reasoning does not serve.
fn combine_windows(answered: Vec<Answers>) -> Vec<Answers> {
    let mut order: Vec<String> = Vec::new();
    let mut merged: BTreeMap<String, Answers> = BTreeMap::new();
    for answer in answered {
        match merged.get_mut(&answer.path) {
            None => {
                order.push(answer.path.clone());
                merged.insert(answer.path.clone(), answer);
            }
            Some(into) => {
                into.bytes += answer.bytes;
                into.cached = into.cached && answer.cached;
                into.windows += 1;
                match (&mut into.values, answer.values) {
                    (Some(kept), Some(next)) => {
                        for (check, value) in next {
                            let slot = kept.entry(check).or_insert(value);
                            *slot = slot.max(value);
                        }
                    }
                    (slot @ None, next) => *slot = next,
                    (Some(_), None) => {}
                }
                if into.error.is_none() {
                    into.error = answer.error;
                }
            }
        }
    }
    order
        .into_iter()
        .filter_map(|path| merged.remove(&path))
        .collect()
}

fn scored(answers: &Answers) -> Value {
    match &answers.values {
        Some(values) => json!({
            "path": answers.path,
            "bytes": answers.bytes,
            "status": "completed",
            "cached": answers.cached,
            "health": smells::health(values),
            "present": smells::present(values)
                .into_iter()
                .map(|(id, value)| json!({ "check": id, "value": value }))
                .collect::<Vec<_>>(),
            "checks": values,
            "windows": answers.windows,
            "basis": answers.note.clone().unwrap_or_else(|| "both versions".into()),
            "warning": answers.error,
        }),
        None => json!({
            "path": answers.path,
            "bytes": answers.bytes,
            "status": "failed",
            "error": answers.error,
        }),
    }
}

/// Health for every file, every directory that holds one, and the tree.
fn run_health(root: &Path, options: &Options, key: Option<&str>) -> Result<(Value, bool), String> {
    let found = discover(root, &options.paths)?;
    let configured = scope::configured_roots();
    let scope = scope::classify(root, &found, configured.as_deref());
    // A file the user names directly is an unambiguous request for it, whatever
    // the scope would have decided.
    let named: BTreeSet<PathBuf> = options
        .paths
        .iter()
        .filter_map(|path| root.join(path).canonicalize().ok())
        .filter(|path| path.is_file())
        .collect();
    let wanted: BTreeSet<String> = named
        .iter()
        .map(|path| {
            path.strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    let paths: Vec<PathBuf> = found
        .iter()
        .filter(|path| {
            let file = path
                .strip_prefix(root)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/");
            options.all
                || wanted.contains(&file)
                || scope
                    .entries
                    .iter()
                    .any(|e| e.path == file && e.status == scope::Status::Included)
        })
        .cloned()
        .collect();
    let mut skipped_files: Vec<Value> = scope
        .entries
        .iter()
        .filter(|e| e.status != scope::Status::Included && !wanted.contains(&e.path))
        .map(|e| json!({ "path": e.path, "reason": e.reason, "status": e.status }))
        .collect();
    let mut subjects = Vec::new();
    let mut unreadable = Vec::new();
    for path in &paths {
        let relative = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/");
        match read_text(path) {
            Ok(source) if !options.all && generated_header(&source) => {
                skipped_files.push(json!({ "path": relative, "reason": "generated" }));
            }
            Ok(source) => {
                let whole = smells::file_request(&relative, &source);
                match within_budget(&whole) {
                    Ok(Some(_)) => subjects.push(Subject {
                        bytes: source.len() as u64,
                        request: whole,
                        path: relative,
                        note: None,
                    }),
                    Ok(None) => match catalog_windows(&relative, &source) {
                        Ok(windows) => {
                            let count = windows.len();
                            for (window, text) in windows {
                                subjects.push(Subject {
                                    bytes: text.len() as u64,
                                    request: smells::file_request(&relative, &text),
                                    path: relative.clone(),
                                    note: Some(format!(
                                        "assessed in {count} windows at declaration boundaries; \
                                         a property of the whole file, such as a god class or \
                                         logic duplicated across windows, is judged within a \
                                         window and is weaker here (window {} of {})",
                                        window.index, window.of
                                    )),
                                });
                            }
                        }
                        Err(e) => unreadable.push(json!({
                            "path": relative, "status": "failed", "error": e
                        })),
                    },
                    Err(e) => unreadable.push(json!({
                        "path": relative, "status": "failed", "error": e
                    })),
                }
            }
            Err(e) => unreadable.push(json!({
                "path": relative, "status": "failed", "error": e
            })),
        }
    }
    if options.dry_run {
        return Ok((
            json!({
                "catalog_version": smells::CATALOG_VERSION, "model": MODEL,
                "requests": subjects.iter().map(|s| &s.request).collect::<Vec<_>>(),
            }),
            false,
        ));
    }
    let count = subjects.len();
    let (answered, usage) = ask_smells(root, key, options.refresh, subjects, count > 4);
    let answers = combine_windows(answered);

    let mut weighted: Vec<(u64, f64)> = Vec::new();
    let mut by_directory: BTreeMap<String, Vec<(u64, f64)>> = BTreeMap::new();
    let mut files: Vec<Value> = Vec::new();
    for answer in &answers {
        if let Some(values) = &answer.values
            && let Some(health) = smells::health(values)
        {
            weighted.push((answer.bytes, health));
            // Every ancestor directory, so a tree can be read at any depth.
            let mut directory = Path::new(&answer.path).parent();
            while let Some(current) = directory {
                by_directory
                    .entry(current.to_string_lossy().replace('\\', "/"))
                    .or_default()
                    .push((answer.bytes, health));
                directory = current.parent();
            }
        }
        files.push(scored(answer));
    }
    files.sort_by(|a, b| {
        let key = |v: &Value| v["health"].as_f64().unwrap_or(f64::MAX);
        key(a).total_cmp(&key(b))
    });
    files.extend(unreadable);
    let failed = files.iter().any(|f| f["status"] == "failed");
    let errors = files.iter().filter(|f| f["status"] == "failed").count();

    let directories: Vec<Value> = by_directory
        .iter()
        .filter(|(name, _)| !name.is_empty())
        .filter_map(|(name, members)| {
            Some(json!({
                "path": name, "files": members.len(),
                "bytes": members.iter().map(|(b, _)| b).sum::<u64>(),
                "health": smells::aggregate(members)?,
            }))
        })
        .collect();

    let (id, created_at) = store::identity()?;
    let mut manifest = json!({
        "schema_version": 3, "id": id, "created_at": created_at, "parent": Value::Null,
        "supercov_version": env!("CARGO_PKG_VERSION"),
        // Which instrument produced this. A reader must never mistake a catalog
        // snapshot for a rubric one: they answer different questions and their
        // numbers are not comparable.
        "instrument": "catalog",
        "catalog_version": smells::CATALOG_VERSION,
        "model": MODEL, "scope": "file", "experimental": true,
        "paths": options.paths.iter()
            .map(|path| path.display().to_string().replace('\\', "/"))
            .collect::<Vec<_>>(),
        "counts": {"files": files.len(), "scored": weighted.len(), "errors": errors,
            "directories": directories.len(), "skipped": skipped_files.len()},
        "scope": scope.summary(),
        "limitation": scope.limitation(),
        "skipped": skipped_files,
        "catalog": smells::described(),
        "health": smells::aggregate(&weighted),
        "bytes": weighted.iter().map(|(b, _)| b).sum::<u64>(),
        "policy": {"present_at": smells::PRESENT_AT,
            "composition": "health is the mean of the catalog answers, computed by this CLI",
            "aggregation": "directory and repository health weight files by size",
            "cutoffs": "none; no threshold in this project has survived calibration",
            "fail_on_finding": false},
        "usage_this_run": usage,
    });
    let recorded = json!({ "files": files, "directories": directories });
    let saved = store::write(root, &id, &manifest, &recorded);
    let mut report = manifest.take();
    if let Err(error) = &saved {
        report["snapshot_warning"] = json!(format!(
            "the assessment completed but no snapshot was saved: {error}"
        ));
    }
    report["directories"] = recorded["directories"].clone();
    report["files"] = recorded["files"].clone();
    report["files_scored"] = json!(weighted.len());
    Ok((report, failed))
}

/// What a change introduced, file by file.
fn run_patch(
    root: &Path,
    range: &changes::Range,
    paths: &[PathBuf],
    refresh: bool,
    all: bool,
    key: Option<&str>,
) -> Result<(Value, bool), String> {
    let collected = changes::collect(root, range, paths)?;
    let configured = scope::configured_roots();
    let changed: Vec<PathBuf> = collected.iter().map(|c| root.join(&c.path)).collect();
    let scope = scope::classify(root, &changed, configured.as_deref());
    let in_scope: BTreeMap<String, scope::Status> = scope
        .entries
        .iter()
        .map(|e| (e.path.clone(), e.status))
        .collect();
    let scope_reasons: BTreeMap<String, String> = scope
        .entries
        .iter()
        .map(|e| (e.path.clone(), e.reason.clone()))
        .collect();
    let mut subjects = Vec::new();
    let mut skipped = Vec::new();
    for change in &collected {
        if !change.reviewable() {
            skipped.push(json!({
                "path": change.path,
                "reason": "a deletion cannot introduce anything",
            }));
            continue;
        }
        if !is_source(Path::new(&change.path)) {
            skipped.push(json!({ "path": change.path, "reason": "not a source file" }));
            continue;
        }
        // The same scope as an assessment, so a reviewer reading `runs patch`
        // and `quality patch` together sees one set of files. A change outside
        // it is reported rather than dropped.
        if !all
            && let Some(entry) = in_scope.get(&change.path)
            && *entry != scope::Status::Included
        {
            skipped.push(json!({
                "path": change.path,
                "reason": scope_reasons.get(&change.path).cloned().unwrap_or_default(),
            }));
            continue;
        }
        if !all && generated_header(&change.after) {
            skipped.push(json!({ "path": change.path, "reason": "generated" }));
            continue;
        }
        // Both whole versions are the validated question. When they do not fit,
        // the patch alone is asked instead and the report says so.
        let whole = smells::change_request(&change.before, &change.after);
        let (request, note) = match within_budget(&whole) {
            Ok(Some(_)) => (whole, None),
            _ if !change.patch.is_empty() => (
                smells::patch_request(&change.patch),
                Some("unified diff only: both versions exceed the request budget".to_owned()),
            ),
            _ => (whole, None),
        };
        subjects.push(Subject {
            bytes: change.after.len() as u64,
            request,
            path: change.path.clone(),
            note,
        });
    }
    let anchors: BTreeMap<String, u32> = collected
        .iter()
        .filter_map(|c| changes::first_added_line(&c.patch).map(|l| (c.path.clone(), l)))
        .collect();
    let count = subjects.len();
    let (answers, usage) = ask_smells(root, key, refresh, subjects, count > 4);
    let mut introduced = 0usize;
    let files: Vec<Value> = answers
        .iter()
        .map(|answer| {
            let mut value = scored(answer);
            value["line"] = json!(anchors.get(&answer.path));
            if let Some(values) = &answer.values {
                let fired = smells::present(values);
                introduced += fired.len();
                // Health is a property of a file, not of a change; what a review
                // reports is which properties appeared.
                value["health"] = Value::Null;
                value["introduced"] = json!(fired.len());
            }
            value
        })
        .collect();
    let mut ordered = files;
    ordered.sort_by(|a, b| {
        b["introduced"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&a["introduced"].as_u64().unwrap_or(0))
            .then_with(|| a["path"].as_str().cmp(&b["path"].as_str()))
    });
    let failed = ordered.iter().any(|f| f["status"] == "failed");
    Ok((
        json!({
            "catalog_version": smells::CATALOG_VERSION,
            "model": MODEL,
            "catalog": smells::described(),
            "scope": scope.summary(),
            "range": range.id(),
            "range_description": range.describe(),
            "head": changes::head(root),
            "changed_files": collected.len(),
            "reviewed_files": count,
            "introduced": introduced,
            "skipped": skipped,
            "usage": usage,
            "files": ordered,
        }),
        failed,
    ))
}

/// The caveat that belongs next to any number this catalog produces.
const SMELL_CAVEAT: &str = "Named-property assessment, experimental and advisory. Each check is a \
model judgment you can verify against the file.\nHealth is arithmetic over those checks, done here \
and not by the model. It correlates strongly with file size.\n";

/// A band, not a decimal. The measured resolution of these judgments is about
/// one point, so `4.4/10` would claim precision nobody observed. The number
/// stays in `--json`, where something has to sort.
fn band(health: Option<f64>) -> &'static str {
    match health {
        Some(h) if h >= 8.0 => "good",
        Some(h) if h >= 5.0 => "fair",
        Some(_) => "weak",
        None => "\u{2014}",
    }
}

/// The three strongest findings and how many more there are.
///
/// Listing all twelve is a wall. Three checks fire on more than half of a real
/// repository, so a full list buries the one finding that is news under the
/// ones that are true of everything. The rest stay a keystroke away in
/// `quality file <path>`.
fn top_findings(present: &[Value]) -> String {
    let named: Vec<String> = present
        .iter()
        .take(3)
        .map(|p| {
            format!(
                "{} {:.2}",
                p["check"].as_str().unwrap_or("?"),
                p["value"].as_f64().unwrap_or(0.0)
            )
        })
        .collect();
    match present.len().saturating_sub(3) {
        0 => named.join(", "),
        more => format!("{}, +{more} more", named.join(", ")),
    }
}

fn human_quality(report: &Value) -> String {
    let empty = Vec::new();
    let mut out = String::new();
    let health = report["health"].as_f64();
    let files = report["files"].as_array().unwrap_or(&empty);
    let scored: Vec<&Value> = files.iter().filter(|f| f["health"].is_number()).collect();
    let count = |low: f64, high: f64| {
        scored
            .iter()
            .filter(|f| {
                let h = f["health"].as_f64().unwrap_or(0.0);
                h >= low && h < high
            })
            .count()
    };
    out.push_str(&format!(
        "Quality {} ({:.1}/10, weighted by size) over {} files, {} bytes.\n",
        band(health),
        health.unwrap_or(0.0),
        scored.len(),
        report["bytes"].as_u64().unwrap_or(0)
    ));
    out.push_str(&format!(
        "  {} good, {} fair, {} weak.\n",
        count(8.0, f64::INFINITY),
        count(5.0, 8.0),
        count(f64::NEG_INFINITY, 5.0)
    ));
    if let Some(limitation) = report["limitation"].as_str() {
        out.push_str(&format!("\n{limitation}\n"));
    }
    let skipped = report["counts"]["skipped"].as_u64().unwrap_or(0);
    if skipped > 0 {
        let by = |reason: &str| {
            report["skipped"]
                .as_array()
                .map(|all| all.iter().filter(|s| s["reason"] == reason).count())
                .unwrap_or(0)
        };
        let scope = &report["scope"];
        out.push_str(&format!(
            "  {} of {skipped} not assessed are test or generated; \
             {} sit under no source root. Add --all to include everything.\n",
            by("test") + by("generated"),
            scope["ambiguous"].as_u64().unwrap_or(0)
        ));
    }
    out.push_str(&format!(
        "Catalog {}, model {}, snapshot {}.\n{SMELL_CAVEAT}\n",
        report["catalog_version"].as_str().unwrap_or("?"),
        report["model"].as_str().unwrap_or("?"),
        report["id"].as_str().unwrap_or("?")
    ));

    let failed: Vec<&Value> = files.iter().filter(|f| f["status"] == "failed").collect();
    let shown = 10.min(scored.len());
    if shown > 0 {
        out.push_str("Weakest:\n");
    }
    for file in scored.iter().take(shown) {
        out.push_str(&format!(
            "  {:>4}  {}\n",
            band(file["health"].as_f64()),
            file["path"].as_str().unwrap_or("?")
        ));
        let present = file["present"].as_array().unwrap_or(&empty);
        if !present.is_empty() {
            out.push_str(&format!("        {}\n", top_findings(present)));
        }
    }
    if scored.len() > shown {
        out.push_str(&format!("  ... and {} more\n", scored.len() - shown));
    }
    for file in failed.iter().take(5) {
        out.push_str(&format!(
            "  error  {}  ({})\n",
            file["path"].as_str().unwrap_or("?"),
            file["error"].as_str().unwrap_or("unknown error")
        ));
    }
    if failed.len() > 5 {
        out.push_str(&format!("  ... and {} more errors\n", failed.len() - 5));
    }
    out.push_str(
        "\nNarrow with `quality gaps`, read one with `quality file <path>`,\nor ask what a change introduced with `quality patch --base origin/main`.\n",
    );
    out
}

fn human_patch(report: &Value, limit: usize) -> String {
    let empty = Vec::new();
    let mut out = String::new();
    out.push_str(SMELL_CAVEAT);
    out.push_str(&format!(
        "Reviewing {}.\n\n",
        report["range_description"].as_str().unwrap_or("a change")
    ));
    let reviewed = report["reviewed_files"].as_u64().unwrap_or(0);
    let introduced = report["introduced"].as_u64().unwrap_or(0);
    if reviewed == 0 {
        out.push_str("No changed source files to review.\n");
        return out;
    }
    if introduced == 0 {
        out.push_str(&format!(
            "Nothing introduced across {reviewed} changed files.\n"
        ));
    }
    let files = report["files"].as_array().unwrap_or(&empty);
    let shown = if limit == 0 { files.len() } else { limit };
    for file in files.iter().take(shown) {
        if file["status"] == "failed" {
            out.push_str(&format!(
                "  failed  {}  ({})\n",
                file["path"].as_str().unwrap_or("?"),
                file["error"].as_str().unwrap_or("unknown error")
            ));
            continue;
        }
        let fired = file["present"].as_array().unwrap_or(&empty);
        if fired.is_empty() {
            continue;
        }
        let basis = file["basis"].as_str().unwrap_or("both versions");
        out.push_str(&format!("{}\n", file["path"].as_str().unwrap_or("?")));
        if basis != "both versions" {
            out.push_str(&format!("  ({basis})\n"));
        }
        for finding in fired {
            out.push_str(&format!(
                "  {:.2}  {}\n",
                finding["value"].as_f64().unwrap_or(0.0),
                finding["check"].as_str().unwrap_or("?")
            ));
        }
        out.push('\n');
    }
    if files.len() > shown {
        out.push_str(&format!(
            "... and {} more changed files\n",
            files.len() - shown
        ));
    }
    let skipped = report["skipped"].as_array().unwrap_or(&empty).len();
    if skipped > 0 {
        out.push_str(&format!("{skipped} changed files not reviewed.\n"));
    }
    out
}

/// GitHub workflow annotations for what a change introduced.
///
/// Printed on stdout, needing no token and posting no comment, the way the
/// coverage patch already does it. A named property is a fact about a file
/// rather than about one line, so each annotation is anchored at the start of
/// the change and says which check fired, never guessing at a line.
fn annotations(report: &Value) -> String {
    let empty = Vec::new();
    let escape = |text: &str| {
        text.replace('%', "%25")
            .replace('\r', "%0D")
            .replace('\n', "%0A")
    };
    let mut out = String::new();
    for file in report["files"].as_array().unwrap_or(&empty) {
        let Some(path) = file["path"].as_str() else {
            continue;
        };
        let line = file["line"].as_u64().unwrap_or(1).max(1);
        for finding in file["present"].as_array().unwrap_or(&empty) {
            let Some(check) = finding["check"].as_str() else {
                continue;
            };
            let value = finding["value"].as_f64().unwrap_or(0.0);
            out.push_str(&format!(
                "::warning file={path},line={line},title=quality: {check}::{}\n",
                escape(&format!(
                    "This change appears to introduce {check} ({value:.2}). \
                     Advisory: a model judgment you can check against the file."
                ))
            ));
        }
    }
    out
}

fn present(view: Value, json: bool) -> Result<bool, String> {
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&view).map_err(|e| e.to_string())?
        );
    } else {
        print!("{}", query::render(&view));
    }
    Ok(false)
}

pub fn command(arguments: Vec<String>) -> ExitCode {
    if arguments.iter().any(|arg| arg == "--help" || arg == "-h") {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let result = (|| -> Result<bool, String> {
        let root = std::env::current_dir()
            .and_then(|p| p.canonicalize())
            .map_err(|e| e.to_string())?;
        match parse(arguments)? {
            Command::Snapshots { json, limit } => present(query::snapshots(&root, limit)?, json),
            Command::Gaps {
                snapshot,
                json,
                limit,
            } => present(query::gaps(&root, snapshot.as_deref(), limit)?, json),
            Command::Scope { json, limit } => {
                let found = discover(&root, &here())?;
                let configured = scope::configured_roots();
                let view = scope::classify(&root, &found, configured.as_deref());
                let shown: Vec<&scope::Entry> = view
                    .entries
                    .iter()
                    .filter(|e| e.status != scope::Status::Included)
                    .take(if limit == 0 { usize::MAX } else { limit })
                    .collect();
                present(
                    json!({
                        "view": "scope",
                        "summary": view.summary(),
                        "limitation": view.limitation(),
                        "not_included": shown,
                        "total_not_included": view.entries.iter()
                            .filter(|e| e.status != scope::Status::Included).count(),
                    }),
                    json,
                )
            }
            Command::Show {
                snapshot,
                json,
                limit,
            } => present(query::show(&root, snapshot.as_deref(), limit)?, json),
            Command::File {
                path,
                snapshot,
                json,
            } => present(query::file(&root, &path, snapshot.as_deref())?, json),
            Command::Diff {
                from,
                to,
                json,
                limit,
            } => present(query::diff(&root, &from, &to, limit)?, json),
            Command::Health(options) => {
                let key = std::env::var("TYPESAFE_API_KEY").ok();
                let (report, failed) = run_health(&root, &options, key.as_deref())?;
                if options.json || options.dry_run {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                    );
                } else {
                    print!("{}", human_quality(&report));
                }
                Ok(failed)
            }
            Command::Patch {
                range,
                paths,
                json,
                refresh,
                limit,
                annotate,
                all,
            } => {
                let key = std::env::var("TYPESAFE_API_KEY").ok();
                let (report, failed) =
                    run_patch(&root, &range, &paths, refresh, all, key.as_deref())?;
                if json {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?
                    );
                } else {
                    print!("{}", human_patch(&report, limit));
                }
                if annotate {
                    print!("{}", annotations(&report));
                }
                Ok(failed)
            }
        }
    })();
    match result {
        Ok(false) => ExitCode::SUCCESS,
        Ok(true) => ExitCode::from(2),
        Err(error) => {
            eprintln!("[supercov] {error}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests;
