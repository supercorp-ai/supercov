//! The coverage formats other tools read, written from the shared run view:
//! LCOV, Cobertura, Istanbul's `coverage-final.json` and its summary, Clover,
//! JaCoCo and SonarQube's generic coverage XML.
//!
//! These exist for compatibility, not as a headline: they let Supercov feed
//! the viewers, hosted services and CI integrations a team already uses. All
//! read the same view the CLI and the gates read, so a consumer's totals match
//! what `check` enforced.
//!
//! Two things are deliberately not done. Supercov records whether a line ran,
//! not how many times, so an export states `1` or `0` and never invents an
//! execution frequency a consumer would display as fact. And MC/DC conditions
//! are not flattened into ordinary branches to fit a simpler model: a consumer
//! would then report condition obligations as branch coverage, which is a
//! different measurement. LCOV has records of its own for them, which are
//! written; the other formats have none, and the evidence stays in the JSON
//! view.

use std::fmt::Write as _;

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::run_view::{ConditionRecord, FileView, Metric, RunView};

/// Escape text for XML character data and attribute values.
fn xml(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            // XML 1.0 cannot carry these at all; dropping them keeps the
            // document parseable rather than emitting bytes no reader accepts.
            c if (c < ' ' && c != '\t' && c != '\n' && c != '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn rate(covered: usize, total: usize) -> f64 {
    if total == 0 {
        // Cobertura has no "not applicable"; 1.0 is the convention for an
        // empty denominator and is what every reader expects.
        return 1.0;
    }
    covered as f64 / total as f64
}

fn counts(view: &RunView, metric: Metric) -> (usize, usize) {
    let metric = view.metric(metric);
    (metric.covered, metric.eligible)
}

/// The export formats, named once so the rest of the product never has to
/// spell them. Supercov emits these formats; it never invokes the tools they
/// are named after, and the packaging audit that enforces that reads this
/// module as the single place allowed to name them.
pub const FORMATS: &str = "lcov|cobertura|istanbul|istanbul-summary|clover|jacoco|sonar|html";

/// Whether a format places statements and functions by their source range,
/// and so needs the text of every file the run measured.
pub fn needs_source(format: &str) -> bool {
    format == "istanbul"
}

/// Render a run in the named format.
///
/// `root` is the checkout the run measured, and `sources` the text of its
/// files by repository-relative path; only a format that [`needs_source`]
/// reads them.
pub fn export(
    view: &RunView,
    format: &str,
    timestamp: u64,
    root: &Path,
    sources: &BTreeMap<String, String>,
) -> Result<String, String> {
    match format {
        "lcov" => Ok(lcov(view)),
        "cobertura" => Ok(cobertura(view, timestamp)),
        "istanbul" => istanbul(view, root, sources),
        "istanbul-summary" => Ok(istanbul_summary(view, root)),
        "clover" => Ok(clover(view, timestamp)),
        "jacoco" => Ok(jacoco(view, timestamp)),
        "sonar" => Ok(sonar(view)),
        // `html` is rendered by `coverage_html`, which also needs verified
        // source text; the CLI routes it before reaching here.
        other => Err(format!(
            "unknown format {other}; Supercov exports {FORMATS}"
        )),
    }
}

/// A file's conditions as LCOV 2's `MCDC` records, and how many of them it
/// counts found and hit.
///
/// LCOV asks of each condition whether it was shown to turn the outcome when
/// it went from false to true, and from true to false, and counts each as
/// one. Supercov counts a condition covered when a pair of evaluations shows
/// it turning the outcome, which is both at once: a covered condition is two
/// hit, one not covered is none, and hit over found is Supercov's own MC/DC.
///
/// A record names its condition by line, the size of its group and its place
/// in the group, so two decisions of one size on one line would be one
/// group to a reader. Where a line holds such decisions, all its conditions
/// are written as one group; otherwise a decision is a group.
fn mcdc_records(conditions: &[ConditionRecord], out: &mut String) -> (usize, usize) {
    let (mut found, mut hit) = (0, 0);
    for line in conditions.chunk_by(|left, right| left.line == right.line) {
        let decisions = line
            .chunk_by(|left, right| left.column == right.column)
            .collect::<Vec<_>>();
        let mut sizes = decisions.iter().map(|each| each.len()).collect::<Vec<_>>();
        sizes.sort_unstable();
        let apart = sizes.windows(2).all(|pair| pair[0] != pair[1]);
        let mut groups = if apart { decisions } else { vec![line] };
        groups.sort_by_key(|group| group.len());
        // A line chunk holds a condition, so each group does.
        groups.into_iter().for_each(|group| {
            group.iter().enumerate().for_each(|(index, condition)| {
                let named = condition
                    .source
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                let named = if named.is_empty() {
                    format!("condition {index}")
                } else {
                    named
                };
                let shown = u8::from(condition.covered);
                found += 2;
                hit += 2 * usize::from(shown);
                for sense in ['t', 'f'] {
                    let _ = writeln!(
                        out,
                        "MCDC:{},{},{sense},{shown},{index},{named}",
                        condition.line,
                        group.len()
                    );
                }
            });
        });
    }
    (found, hit)
}

/// An LCOV tracefile: line, function and branch records, and LCOV 2's
/// `MCDC` records, which a reader of an older format passes over.
pub fn lcov(view: &RunView) -> String {
    let mut out = String::new();
    for file in &view.files {
        let _ = writeln!(out, "TN:");
        let _ = writeln!(out, "SF:{}", file.file);
        for function in &file.functions {
            let _ = writeln!(out, "FN:{},{}", function.line, function.name);
        }
        for function in &file.functions {
            // Hit or not hit. Supercov does not count invocations, and writing
            // a made-up frequency here would be displayed as one.
            let _ = writeln!(out, "FNDA:{},{}", u8::from(function.covered), function.name);
        }
        let (covered_functions, functions) = file.metric(Metric::Functions).counts();
        let _ = writeln!(out, "FNF:{functions}");
        let _ = writeln!(out, "FNH:{covered_functions}");
        for branch in &file.branches {
            let _ = writeln!(
                out,
                "BRDA:{},{},{},{}",
                branch.line,
                branch.block,
                branch.index,
                if branch.taken { "1" } else { "-" }
            );
        }
        let (covered_branches, branches) = file.metric(Metric::Branches).counts();
        let _ = writeln!(out, "BRF:{branches}");
        let _ = writeln!(out, "BRH:{covered_branches}");
        let (conditions, shown) = mcdc_records(&file.conditions, &mut out);
        if conditions > 0 {
            let _ = writeln!(out, "MCF:{conditions}");
            let _ = writeln!(out, "MCH:{shown}");
        }
        for (line, covered) in file.line_hits() {
            let _ = writeln!(out, "DA:{line},{}", u8::from(covered));
        }
        let (covered_lines, lines) = file.metric(Metric::Lines).counts();
        let _ = writeln!(out, "LF:{lines}");
        let _ = writeln!(out, "LH:{covered_lines}");
        let _ = writeln!(out, "end_of_record");
    }
    out
}

/// A Cobertura XML report.
pub fn cobertura(view: &RunView, timestamp: u64) -> String {
    let (covered_lines, lines) = counts(view, Metric::Lines);
    let (covered_branches, branches) = counts(view, Metric::Branches);
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<coverage line-rate=\"{:.4}\" branch-rate=\"{:.4}\" lines-covered=\"{covered_lines}\" lines-valid=\"{lines}\" branches-covered=\"{covered_branches}\" branches-valid=\"{branches}\" complexity=\"0\" version=\"{}\" timestamp=\"{timestamp}\">",
        rate(covered_lines, lines),
        rate(covered_branches, branches),
        xml(env!("CARGO_PKG_VERSION")),
    );
    // A single relative source root keeps filenames resolvable by readers that
    // join them onto a checkout, and leaks no absolute path from this machine.
    out.push_str("  <sources>\n    <source>.</source>\n  </sources>\n  <packages>\n");
    let _ = writeln!(
        out,
        "    <package name=\"\" line-rate=\"{:.4}\" branch-rate=\"{:.4}\" complexity=\"0\">",
        rate(covered_lines, lines),
        rate(covered_branches, branches),
    );
    out.push_str("      <classes>\n");
    for file in &view.files {
        let (file_covered, file_lines) = file.metric(Metric::Lines).counts();
        let (branch_covered, branch_total) = file.metric(Metric::Branches).counts();
        let _ = writeln!(
            out,
            "        <class name=\"{}\" filename=\"{}\" line-rate=\"{:.4}\" branch-rate=\"{:.4}\" complexity=\"0\">",
            xml(&file.file),
            xml(&file.file),
            rate(file_covered, file_lines),
            rate(branch_covered, branch_total),
        );
        out.push_str("          <methods/>\n          <lines>\n");
        let branches = branches_by_line(file);
        for (line, covered) in file.line_hits() {
            let Some((taken, untaken)) = branches.get(&line).copied() else {
                let _ = writeln!(
                    out,
                    "            <line number=\"{line}\" hits=\"{}\"/>",
                    u8::from(covered)
                );
                continue;
            };
            let total = taken + untaken;
            let _ = writeln!(
                out,
                "            <line number=\"{line}\" hits=\"{}\" branch=\"true\" condition-coverage=\"{}% ({taken}/{total})\"/>",
                u8::from(covered),
                (rate(taken, total) * 100.0).round() as u64,
            );
        }
        out.push_str("          </lines>\n        </class>\n");
    }
    out.push_str("      </classes>\n    </package>\n  </packages>\n</coverage>\n");
    out
}

/// A file's absolute path in the checkout, which is how Istanbul's own
/// reports key their files and what their readers resolve. On Windows it is
/// the plain `C:\...` form Node writes, never a verbatim `\\?\` one, which
/// no reader would match to the file.
fn absolute(root: &Path, file: &str) -> String {
    crate::workspace::simplified(
        file.split('/')
            .fold(root.to_path_buf(), |path, part| path.join(part)),
    )
    .to_string_lossy()
    .into_owned()
}

/// How a frontend counts the column it records a point or branch at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Columns {
    /// JavaScript and TypeScript: from 1, in UTF-16 units.
    Utf16FromOne,
    /// Python, Ruby and Rust: from 0, in bytes.
    BytesFromZero,
    /// Go, Java and Kotlin: from 1, in bytes.
    BytesFromOne,
}

/// The column convention of the frontend that measures `file`.
fn columns_of(file: &str) -> Columns {
    match file.rsplit_once('.').map_or("", |(_, extension)| extension) {
        "py" | "pyi" | "rb" | "rs" => Columns::BytesFromZero,
        "go" | "java" | "kt" => Columns::BytesFromOne,
        _ => Columns::Utf16FromOne,
    }
}

/// A file's text with where each of its lines starts, to place what a run
/// recorded in it.
struct Placed<'a> {
    text: &'a str,
    starts: Vec<usize>,
    columns: Columns,
}

impl<'a> Placed<'a> {
    fn new(text: &'a str, columns: Columns) -> Self {
        let starts = std::iter::once(0)
            .chain(text.match_indices('\n').map(|(at, _)| at + 1))
            .collect();
        Placed {
            text,
            starts,
            columns,
        }
    }

    /// A byte offset as Istanbul states a position: the 1-based line and the
    /// 0-based column in UTF-16 units, which is how JavaScript counts.
    fn position(&self, offset: usize) -> Value {
        let line = self.starts.partition_point(|start| *start <= offset) - 1;
        let column = self.text[self.starts[line]..offset].encode_utf16().count();
        json!({ "line": line + 1, "column": column })
    }

    /// The byte offset of a recorded line and column, past any whitespace
    /// there: a frontend that trims the source it records may start its
    /// range before it. `None` when the line no longer reaches that far.
    fn offset(&self, line: usize, column: usize) -> Option<usize> {
        let start = *self.starts.get(line.checked_sub(1)?)?;
        let next = self.starts.get(line).copied().unwrap_or(self.text.len());
        let text = &self.text[start..next];
        let within = match self.columns {
            Columns::BytesFromZero => column,
            Columns::BytesFromOne => column.checked_sub(1)?,
            Columns::Utf16FromOne => {
                let wanted = column.checked_sub(1)?;
                let mut units = 0;
                let mut found = None;
                for (at, character) in text.char_indices() {
                    if units == wanted {
                        found = Some(at);
                        break;
                    }
                    units += character.len_utf16();
                }
                found.or((units == wanted).then_some(text.len()))?
            }
        };
        text.get(within..)?;
        let rest = &self.text[start + within..];
        Some(start + within + rest.len() - rest.trim_start().len())
    }

    /// Where `source` sits, as a byte range: at the recorded line and column,
    /// and only if the file still holds that source there.
    fn find(&self, line: usize, column: usize, source: &str) -> Option<(usize, usize)> {
        let at = self.offset(line, column)?;
        (!source.is_empty() && self.text[at..].starts_with(source))
            .then_some((at, at + source.len()))
    }

    fn stale(file: &str, line: usize) -> String {
        format!(
            "{file}:{line} no longer holds the source the run recorded there; run the tests again to export it"
        )
    }

    fn range(&self, file: &str, line: usize, column: usize, source: &str) -> Result<Value, String> {
        let (start, end) = self
            .find(line, column, source)
            .ok_or_else(|| Self::stale(file, line))?;
        Ok(json!({ "start": self.position(start), "end": self.position(end) }))
    }

    /// A branch's range. A frontend records some branches by a name, such as
    /// `try / catch`, or by a part, such as a loop's iterable, rather than by
    /// their source: those are placed at their start, with no extent, rather
    /// than given one Supercov never recorded.
    fn branch(
        &self,
        file: &str,
        line: usize,
        column: usize,
        source: &str,
    ) -> Result<Value, String> {
        let at = self
            .offset(line, column)
            .ok_or_else(|| Self::stale(file, line))?;
        let end = if !source.is_empty() && self.text[at..].starts_with(source) {
            at + source.len()
        } else {
            at
        };
        Ok(json!({ "start": self.position(at), "end": self.position(end) }))
    }
}

/// Istanbul's names for the kinds of branch it has, so a reader that treats
/// one kind specially sees it; a kind Istanbul has no name for keeps
/// Supercov's.
fn istanbul_branch_type(kind: &str) -> &str {
    match kind {
        "logical-value" => "binary-expr",
        "default-value" => "default-arg",
        other => other,
    }
}

fn istanbul_file(root: &Path, file: &FileView, text: &str) -> Result<Value, String> {
    let placed = Placed::new(text, columns_of(&file.file));
    let (mut statements, mut hits) = (Map::new(), Map::new());
    let (mut functions, mut calls) = (Map::new(), Map::new());
    for point in &file.points {
        let range = placed.range(&file.file, point.line, point.column, &point.source)?;
        let shown = u8::from(point.covered);
        if point.function {
            let key = functions.len().to_string();
            // A function with no name is named as Istanbul names one, by its
            // place in the map, which is how readers tell it is anonymous.
            let name = point
                .name
                .clone()
                .unwrap_or_else(|| format!("(anonymous_{key})"));
            functions.insert(
                key.clone(),
                json!({ "name": name, "decl": range, "loc": range, "line": point.line }),
            );
            calls.insert(key, json!(shown));
        } else {
            let key = statements.len().to_string();
            statements.insert(key.clone(), range);
            hits.insert(key, json!(shown));
        }
    }
    let (mut branches, mut taken) = (Map::new(), Map::new());
    for (index, branch) in file.branch_sites.iter().enumerate() {
        let range = placed.branch(&file.file, branch.line, branch.column, &branch.source)?;
        // Supercov records where a branch is, not where each of its
        // alternatives is, so each alternative is placed at its branch.
        let locations = vec![range.clone(); branch.taken.len()];
        branches.insert(
            index.to_string(),
            json!({
                "loc": range,
                "type": istanbul_branch_type(&branch.kind),
                "locations": locations,
                "line": branch.line,
            }),
        );
        taken.insert(
            index.to_string(),
            Value::Array(
                branch
                    .taken
                    .iter()
                    .map(|each| json!(u8::from(*each)))
                    .collect(),
            ),
        );
    }
    Ok(json!({
        "path": absolute(root, &file.file),
        "statementMap": statements,
        "fnMap": functions,
        "branchMap": branches,
        "s": hits,
        "f": calls,
        "b": taken,
    }))
}

/// Istanbul's `coverage-final.json`: every statement, function and branch
/// placed by its source range, keyed by absolute path as Istanbul keys them.
///
/// A count is `1` or `0`, never a frequency. Istanbul has no record for an
/// MC/DC condition, so conditions are left out rather than shown as branches.
pub fn istanbul(
    view: &RunView,
    root: &Path,
    sources: &BTreeMap<String, String>,
) -> Result<String, String> {
    let mut out = Map::new();
    for file in &view.files {
        let text = sources.get(&file.file).ok_or_else(|| {
            format!(
                "{} could not be read from this checkout, and Istanbul places every statement by its source",
                file.file
            )
        })?;
        out.insert(absolute(root, &file.file), istanbul_file(root, file, text)?);
    }
    Ok(format!("{}\n", Value::Object(out)))
}

fn istanbul_count(covered: usize, total: usize) -> Value {
    // Istanbul states an empty denominator as 100% and rounds to hundredths.
    let pct = if total == 0 {
        100.0
    } else {
        (covered as f64 * 10_000.0 / total as f64).round() / 100.0
    };
    json!({ "total": total, "covered": covered, "skipped": 0, "pct": pct })
}

fn istanbul_summary_of(metric: impl Fn(Metric) -> (usize, usize)) -> Value {
    let mut entry = Map::new();
    for (name, each) in [
        ("lines", Metric::Lines),
        ("statements", Metric::Statements),
        ("functions", Metric::Functions),
        ("branches", Metric::Branches),
    ] {
        let (covered, total) = metric(each);
        entry.insert(name.into(), istanbul_count(covered, total));
    }
    Value::Object(entry)
}

/// Istanbul's `coverage-summary.json`: the run's totals and each file's, as
/// Supercov counted them.
pub fn istanbul_summary(view: &RunView, root: &Path) -> String {
    let mut out = Map::new();
    out.insert(
        "total".into(),
        istanbul_summary_of(|metric| counts(view, metric)),
    );
    for file in &view.files {
        out.insert(
            absolute(root, &file.file),
            istanbul_summary_of(|metric| file.metric(metric).counts()),
        );
    }
    format!("{}\n", Value::Object(out))
}

/// Taken and not-taken alternatives of the branches on each measured line.
///
/// A branch can sit on a line that is not itself measured: a later line of a
/// statement written across several, or a parameter's default in a signature
/// that spans lines. A format that states branches by line would drop it, and
/// its branch totals would fall short of Supercov's. It is stated on the
/// nearest measured line above it, where its statement starts, or on the
/// first measured line where there is none above.
fn branches_by_line(file: &FileView) -> BTreeMap<usize, (usize, usize)> {
    let mut lines = BTreeMap::<usize, (usize, usize)>::new();
    let measured = &file.measured_lines;
    for branch in &file.branches {
        let at = measured.partition_point(|line| *line <= branch.line);
        let Some(line) = at
            .checked_sub(1)
            .map(|index| measured[index])
            .or_else(|| measured.first().copied())
        else {
            continue;
        };
        let entry = lines.entry(line).or_default();
        if branch.taken {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
    lines
}

fn clover_metrics(metric: impl Fn(Metric) -> (usize, usize), extra: &str) -> String {
    let (statements_covered, statements) = metric(Metric::Statements);
    let (branches_covered, branches) = metric(Metric::Branches);
    let (functions_covered, functions) = metric(Metric::Functions);
    let (_, lines) = metric(Metric::Lines);
    format!(
        "<metrics statements=\"{statements}\" coveredstatements=\"{statements_covered}\" conditionals=\"{branches}\" coveredconditionals=\"{branches_covered}\" methods=\"{functions}\" coveredmethods=\"{functions_covered}\" elements=\"{}\" coveredelements=\"{}\" complexity=\"0\" loc=\"{lines}\" ncloc=\"{lines}\"{extra}/>",
        statements + branches + functions,
        statements_covered + branches_covered + functions_covered,
    )
}

/// A Clover XML report, shaped as Istanbul writes one: a `stmt` line for
/// each measured line and a `cond` line, with its taken and untaken
/// alternatives, where branches are.
pub fn clover(view: &RunView, timestamp: u64) -> String {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let _ = writeln!(
        out,
        "<coverage generated=\"{timestamp}\" clover=\"3.2.0\">\n  <project timestamp=\"{timestamp}\" name=\"All files\">"
    );
    let _ = writeln!(
        out,
        "    {}",
        clover_metrics(
            |metric| counts(view, metric),
            &format!(
                " packages=\"1\" files=\"{0}\" classes=\"{0}\"",
                view.files.len()
            ),
        )
    );
    for file in &view.files {
        let name = file.file.rsplit('/').next().unwrap_or(&file.file);
        let _ = writeln!(
            out,
            "    <file name=\"{}\" path=\"{}\">\n      {}",
            xml(name),
            xml(&file.file),
            clover_metrics(|metric| file.metric(metric).counts(), ""),
        );
        let branches = branches_by_line(file);
        for (line, covered) in file.line_hits() {
            match branches.get(&line) {
                Some((taken, untaken)) => {
                    let _ = writeln!(
                        out,
                        "      <line num=\"{line}\" count=\"{}\" type=\"cond\" truecount=\"{taken}\" falsecount=\"{untaken}\"/>",
                        u8::from(covered)
                    );
                }
                None => {
                    let _ = writeln!(
                        out,
                        "      <line num=\"{line}\" count=\"{}\" type=\"stmt\"/>",
                        u8::from(covered)
                    );
                }
            }
        }
        out.push_str("    </file>\n");
    }
    out.push_str("  </project>\n</coverage>\n");
    out
}

fn jacoco_counter(out: &mut String, indent: &str, kind: &str, (covered, total): (usize, usize)) {
    let _ = writeln!(
        out,
        "{indent}<counter type=\"{kind}\" missed=\"{}\" covered=\"{covered}\"/>",
        total - covered
    );
}

/// The counters JaCoCo states at every level. Supercov counts no bytecode
/// instructions, so an instruction here is a measured line, the unit the
/// line records below count in.
fn jacoco_counters(out: &mut String, indent: &str, metric: impl Fn(Metric) -> (usize, usize)) {
    jacoco_counter(out, indent, "INSTRUCTION", metric(Metric::Lines));
    jacoco_counter(out, indent, "BRANCH", metric(Metric::Branches));
    jacoco_counter(out, indent, "LINE", metric(Metric::Lines));
    jacoco_counter(out, indent, "METHOD", metric(Metric::Functions));
}

/// A JaCoCo XML report: a package for each directory, holding a class and a
/// source file for each file in it.
///
/// A line states one instruction, missed or covered, rather than a count of
/// bytecode Supercov never measured, and its branches as taken and untaken
/// alternatives.
pub fn jacoco(view: &RunView, timestamp: u64) -> String {
    let mut out = String::new();
    // Not `standalone`, as JaCoCo writes it: its document has no whitespace
    // between elements, and this one is indented, which a standalone
    // document whose elements the DTD declares may not be.
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<!DOCTYPE report PUBLIC \"-//JACOCO//DTD Report 1.1//EN\" \"report.dtd\">\n");
    let _ = writeln!(
        out,
        "<report name=\"Supercov\">\n  <sessioninfo id=\"supercov\" start=\"{0}\" dump=\"{0}\"/>",
        timestamp * 1000
    );
    let mut packages = BTreeMap::<&str, Vec<&FileView>>::new();
    for file in &view.files {
        let directory = file
            .file
            .rsplit_once('/')
            .map_or("", |(directory, _)| directory);
        packages.entry(directory).or_default().push(file);
    }
    for (package, files) in packages {
        let _ = writeln!(out, "  <package name=\"{}\">", xml(package));
        // A package holds the files that made it, so it is never empty.
        files.iter().for_each(|file| {
            let name = file.file.rsplit('/').next().unwrap_or(&file.file);
            let class = file
                .file
                .rsplit_once('.')
                .map_or(file.file.as_str(), |(stem, _)| stem);
            let _ = writeln!(
                out,
                "    <class name=\"{}\" sourcefilename=\"{}\">",
                xml(class),
                xml(name)
            );
            for function in &file.functions {
                let _ = writeln!(
                    out,
                    "      <method name=\"{}\" desc=\"()\" line=\"{}\">",
                    xml(&function.name),
                    function.line
                );
                jacoco_counter(
                    &mut out,
                    "        ",
                    "METHOD",
                    (usize::from(function.covered), 1),
                );
                out.push_str("      </method>\n");
            }
            jacoco_counter(
                &mut out,
                "      ",
                "METHOD",
                file.metric(Metric::Functions).counts(),
            );
            out.push_str("    </class>\n");
        });
        files.iter().for_each(|file| {
            let name = file.file.rsplit('/').next().unwrap_or(&file.file);
            let _ = writeln!(out, "    <sourcefile name=\"{}\">", xml(name));
            let branches = branches_by_line(file);
            for (line, covered) in file.line_hits() {
                let (taken, untaken) = branches.get(&line).copied().unwrap_or_default();
                let _ = writeln!(
                    out,
                    "      <line nr=\"{line}\" mi=\"{}\" ci=\"{}\" mb=\"{untaken}\" cb=\"{taken}\"/>",
                    u8::from(!covered),
                    u8::from(covered)
                );
            }
            jacoco_counters(&mut out, "      ", |metric| file.metric(metric).counts());
            out.push_str("    </sourcefile>\n");
        });
        let sum = |metric: Metric| {
            files.iter().fold((0, 0), |(covered, total), file| {
                let (each_covered, each_total) = file.metric(metric).counts();
                (covered + each_covered, total + each_total)
            })
        };
        jacoco_counters(&mut out, "    ", sum);
        out.push_str("  </package>\n");
    }
    jacoco_counters(&mut out, "  ", |metric| counts(view, metric));
    out.push_str("</report>\n");
    out
}

/// SonarQube's generic coverage XML: each measured line, covered or not, with
/// the alternatives of the branches on it.
pub fn sonar(view: &RunView) -> String {
    let mut out = String::from("<coverage version=\"1\">\n");
    for file in &view.files {
        let _ = writeln!(out, "  <file path=\"{}\">", xml(&file.file));
        let branches = branches_by_line(file);
        for (line, covered) in file.line_hits() {
            match branches.get(&line) {
                Some((taken, untaken)) => {
                    let _ = writeln!(
                        out,
                        "    <lineToCover lineNumber=\"{line}\" covered=\"{covered}\" branchesToCover=\"{}\" coveredBranches=\"{taken}\"/>",
                        taken + untaken
                    );
                }
                None => {
                    let _ = writeln!(
                        out,
                        "    <lineToCover lineNumber=\"{line}\" covered=\"{covered}\"/>"
                    );
                }
            }
        }
        out.push_str("  </file>\n");
    }
    out.push_str("</coverage>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_view::{
        Applicability, BranchRecord, BranchSite, FileView, FunctionRecord, MetricView, PointSite,
        RUN_VIEW_SCHEMA_VERSION, RunView,
    };
    use std::collections::BTreeSet;

    #[test]
    fn xml_escapes_what_markup_reads_and_drops_what_it_cannot_carry() {
        assert_eq!(xml(""), "");
        assert_eq!(
            xml("a & b < c > d \" e ' f\u{1}\tg"),
            "a &amp; b &lt; c &gt; d &quot; e &apos; f\tg"
        );
    }

    /// A run with no file exports a report of nothing in each format.
    #[test]
    fn a_view_of_no_file_exports_nothing_measured() {
        let mut view = fixture();
        view.files.clear();
        assert_eq!(lcov(&view), "");
        assert!(cobertura(&view, 0).contains("<packages>"));
        let root = Path::new("/w");
        assert_eq!(istanbul(&view, root, &BTreeMap::new()).unwrap(), "{}\n");
        let summary: Value = serde_json::from_str(&istanbul_summary(&view, root)).unwrap();
        assert_eq!(summary.as_object().unwrap().len(), 1, "only the total");
        assert!(clover(&view, 0).contains("files=\"0\""));
        let written = jacoco(&view, 0);
        assert!(!written.contains("<package"), "{written}");
        assert!(
            written.ends_with("<counter type=\"METHOD\" missed=\"0\" covered=\"1\"/>\n</report>\n")
        );
        assert_eq!(sonar(&view), "<coverage version=\"1\">\n</coverage>\n");
    }

    /// A branch whose place no longer holds is refused like a statement's,
    /// even when every statement still holds.
    #[test]
    fn a_branch_the_file_no_longer_holds_is_not_placed() {
        let mut view = fixture();
        view.files[0].branch_sites[0].line = 9;
        let error = istanbul(&view, Path::new("/w"), &sources()).unwrap_err();
        assert!(
            error.starts_with("src/a & b.ts:9 no longer holds"),
            "{error}"
        );
    }

    fn metric(metric: Metric, covered: usize, eligible: usize) -> MetricView {
        MetricView {
            metric,
            covered,
            eligible,
            applicability: if eligible == 0 {
                Applicability::NotApplicable
            } else {
                Applicability::Measured
            },
        }
    }

    fn fixture() -> RunView {
        RunView {
            schema_version: RUN_VIEW_SCHEMA_VERSION,
            run: "run_1".into(),
            generated_at: "now".into(),
            suite_passed: true,
            stale: false,
            stale_reasons: Vec::new(),
            complete: true,
            limitations: Vec::new(),
            totals: vec![
                metric(Metric::Lines, 3, 4),
                metric(Metric::Statements, 1, 2),
                metric(Metric::Branches, 1, 2),
                metric(Metric::Functions, 1, 1),
            ],
            files: vec![FileView {
                file: "src/a & b.ts".into(),
                metrics: vec![
                    metric(Metric::Lines, 3, 4),
                    metric(Metric::Statements, 1, 2),
                    metric(Metric::Branches, 1, 2),
                    metric(Metric::Functions, 1, 1),
                ],
                measured_lines: vec![1, 2, 3, 4],
                uncovered_lines: vec![4],
                missing_branches: Vec::new(),
                missing_conditions: Vec::new(),
                functions: vec![FunctionRecord {
                    line: 1,
                    name: "run".into(),
                    covered: true,
                }],
                branches: vec![
                    BranchRecord {
                        line: 2,
                        block: 0,
                        index: 0,
                        taken: true,
                    },
                    BranchRecord {
                        line: 2,
                        block: 0,
                        index: 1,
                        taken: false,
                    },
                ],
                // Line 2 holds `a && b` with `a` shown and `b` not; line 3 two
                // decisions of two conditions each, which a reader would take
                // for one group.
                conditions: [
                    (2, 4, 0, true, "a"),
                    (2, 4, 1, false, "b ==\n  1"),
                    (3, 2, 0, true, "c"),
                    (3, 2, 1, true, "d"),
                    (3, 20, 0, false, "e"),
                    (3, 20, 1, true, "f(1, 2)"),
                ]
                .into_iter()
                .map(|(line, column, index, covered, source)| ConditionRecord {
                    line,
                    column,
                    index,
                    covered,
                    source: source.into(),
                })
                .collect(),
                // Placed in SOURCE, columns as JavaScript counts them: from 1,
                // in UTF-16 units.
                points: vec![
                    PointSite {
                        function: true,
                        line: 1,
                        column: 1,
                        source: SOURCE.trim_end().into(),
                        name: Some("run".into()),
                        covered: true,
                    },
                    PointSite {
                        function: false,
                        line: 2,
                        column: 20,
                        source: "return 1;".into(),
                        name: None,
                        covered: true,
                    },
                    PointSite {
                        function: false,
                        line: 3,
                        column: 3,
                        source: "return c && d || e && f(1, 2);".into(),
                        name: None,
                        covered: false,
                    },
                ],
                branch_sites: vec![BranchSite {
                    kind: "if".into(),
                    line: 2,
                    column: 3,
                    source: "if (a && b == 1) return 1;".into(),
                    taken: vec![true, false],
                }],
            }],
            source_neighbourhoods: BTreeSet::new(),
        }
    }

    /// The text of the fixture's one file.
    const SOURCE: &str = "export function run(a, b) {\n  if (a && b == 1) return 1;\n  return c && d || e && f(1, 2);\n}\n";

    fn sources() -> BTreeMap<String, String> {
        BTreeMap::from([("src/a & b.ts".to_owned(), SOURCE.to_owned())])
    }

    /// A line where bytes and UTF-16 units differ before the statement,
    /// placed from each frontend's convention, lands on one range.
    #[test]
    fn a_point_is_placed_by_its_frontend_s_column_convention() {
        let text = "x = 1\nif \u{e9}\u{1f600}: g()\n";
        let bytes = text.lines().nth(1).unwrap().find("g()").unwrap();
        let units = "if \u{e9}\u{1f600}: ".encode_utf16().count();
        assert_ne!(bytes, units);
        let expected = Some((6 + bytes, 6 + bytes + 3));
        for (columns, column) in [
            (Columns::Utf16FromOne, units + 1),
            (Columns::BytesFromZero, bytes),
            (Columns::BytesFromOne, bytes + 1),
        ] {
            let placed = Placed::new(text, columns);
            assert_eq!(placed.find(2, column, "g()"), expected, "{columns:?}");
            assert_eq!(
                placed.position(6 + bytes),
                json!({ "line": 2, "column": units })
            );
        }
        // Read with another frontend's convention, the column is not at the
        // statement, and nothing is placed.
        assert_eq!(
            Placed::new(text, Columns::BytesFromZero).find(2, units + 1, "g()"),
            None
        );
        assert_eq!(columns_of("src/a.py"), Columns::BytesFromZero);
        assert_eq!(columns_of("lib/a.rb"), Columns::BytesFromZero);
        assert_eq!(columns_of("src/lib.rs"), Columns::BytesFromZero);
        assert_eq!(columns_of("calc.go"), Columns::BytesFromOne);
        assert_eq!(columns_of("src/main/java/A.java"), Columns::BytesFromOne);
        assert_eq!(columns_of("src/main/kotlin/A.kt"), Columns::BytesFromOne);
        assert_eq!(columns_of("src/a.tsx"), Columns::Utf16FromOne);
        assert_eq!(columns_of("Makefile"), Columns::Utf16FromOne);
    }

    /// A frontend that trims what it records may start the range before the
    /// whitespace it trimmed, even on the line before.
    #[test]
    fn whitespace_before_recorded_source_is_passed_over() {
        let placed = Placed::new("#[a]\n    f();\n", Columns::BytesFromZero);
        assert_eq!(placed.find(1, 4, "f();"), Some((9, 13)));
        assert_eq!(placed.find(2, 0, "f();"), Some((9, 13)));
    }

    /// Of two occurrences on a line, the one at the recorded column.
    #[test]
    fn a_repeated_statement_is_the_one_at_its_column() {
        let placed = Placed::new("f(); f();\n", Columns::Utf16FromOne);
        assert_eq!(placed.find(1, 1, "f();"), Some((0, 4)));
        assert_eq!(placed.find(1, 6, "f();"), Some((5, 9)));
        // The space before the second is passed over, as a trimmed range's.
        assert_eq!(placed.find(1, 5, "f();"), Some((5, 9)));
        assert_eq!(placed.find(1, 2, "f();"), None);
    }

    /// A branch recorded by a name or a part is placed at its start, with no
    /// extent; one recorded by its source spans it.
    #[test]
    fn a_branch_without_its_source_is_placed_at_its_start() {
        let placed = Placed::new("  try { a(); } catch { b(); }\n", Columns::Utf16FromOne);
        let at = json!({ "line": 1, "column": 2 });
        assert_eq!(
            placed.branch("f.js", 1, 3, "try / catch").unwrap(),
            json!({ "start": at, "end": at })
        );
        assert_eq!(
            placed.branch("f.js", 1, 3, "try { a(); }").unwrap(),
            json!({ "start": at, "end": { "line": 1, "column": 14 } })
        );
        assert!(
            placed
                .branch("f.js", 2, 3, "try")
                .unwrap_err()
                .starts_with("f.js:2 no longer holds")
        );
    }

    /// A recorded place the text cannot hold, in each convention, is no
    /// offset: line 0, a column before the first, one past the line's end, or
    /// one inside a character. A column just past the last character is the
    /// line's end, where a frontend records the end of what it measured.
    #[test]
    fn a_place_the_line_cannot_hold_has_no_offset() {
        let text = "\u{e9}\u{1f600}x\nlast";
        let utf16 = Placed::new(text, Columns::Utf16FromOne);
        assert_eq!(utf16.offset(0, 1), None);
        // é is one unit and 😀 two, so unit 2 is inside the emoji.
        assert_eq!(utf16.offset(1, 3), None);
        assert_eq!(utf16.offset(1, 4), Some(6));
        // The line's five units end at its line feed; past that is nothing.
        assert_eq!(utf16.offset(2, 5), Some(text.len()));
        assert_eq!(utf16.offset(2, 6), None);
        let zero = Placed::new(text, Columns::BytesFromZero);
        assert_eq!(zero.offset(1, 1), None, "inside é");
        assert_eq!(zero.offset(1, 99), None, "past the line");
        assert_eq!(zero.offset(1, 6), Some(6));
        let one = Placed::new(text, Columns::BytesFromOne);
        assert_eq!(one.offset(1, 0), None);
        assert_eq!(one.offset(1, 7), Some(6));
    }

    /// A branch recorded with no source at all is placed at its start, as
    /// one recorded by a name is.
    #[test]
    fn a_branch_with_no_source_has_no_extent() {
        let placed = Placed::new("a ?? b\n", Columns::Utf16FromOne);
        let at = json!({ "line": 1, "column": 0 });
        assert_eq!(
            placed.branch("f.js", 1, 1, "").unwrap(),
            json!({ "start": at, "end": at })
        );
    }

    /// The kinds Istanbul names differently carry Istanbul's name, which its
    /// readers key on; the rest keep Supercov's.
    #[test]
    fn a_branch_kind_istanbul_names_differently_carries_its_name() {
        assert_eq!(istanbul_branch_type("logical-value"), "binary-expr");
        assert_eq!(istanbul_branch_type("default-value"), "default-arg");
        assert_eq!(istanbul_branch_type("if"), "if");
        let mut view = fixture();
        view.files[0].branch_sites[0].kind = "logical-value".into();
        let parsed: Value =
            serde_json::from_str(&istanbul(&view, Path::new("/w"), &sources()).unwrap()).unwrap();
        let file = &parsed[absolute(Path::new("/w"), "src/a & b.ts")];
        assert_eq!(file["branchMap"]["0"]["type"], json!("binary-expr"));
    }

    /// A function with no name is named by its place in the map, as Istanbul
    /// names one, so readers that match functions by name still find it.
    #[test]
    fn an_unnamed_function_is_named_as_istanbul_names_one() {
        let mut view = fixture();
        view.files[0].points[0].name = None;
        let parsed: Value =
            serde_json::from_str(&istanbul(&view, Path::new("/w"), &sources()).unwrap()).unwrap();
        let file = &parsed[absolute(Path::new("/w"), "src/a & b.ts")];
        assert_eq!(file["fnMap"]["0"]["name"], json!("(anonymous_0)"));
    }

    /// A file at the checkout's root is in JaCoCo's default package, and one
    /// with no extension is its own class name.
    #[test]
    fn jacoco_places_a_root_file_in_the_default_package() {
        let mut view = fixture();
        view.files[0].file = "Makefile".into();
        let written = jacoco(&view, 0);
        for expected in [
            "<package name=\"\">",
            "<class name=\"Makefile\" sourcefilename=\"Makefile\">",
            "<sourcefile name=\"Makefile\">",
        ] {
            assert!(written.contains(expected), "{expected}\n{written}");
        }
        assert!(clover(&view, 0).contains("<file name=\"Makefile\" path=\"Makefile\">"));
    }

    /// Source the line no longer holds is not placed, and the export says
    /// which file and line to measure again rather than guessing.
    #[test]
    fn source_no_longer_in_the_file_is_not_placed() {
        let placed = Placed::new("a();\nb();\n", Columns::Utf16FromOne);
        assert_eq!(placed.find(1, 1, "b();"), None);
        assert_eq!(placed.find(3, 1, "b();"), None);
        assert_eq!(placed.find(1, 9, "b();"), None);
        assert_eq!(placed.find(1, 0, "a();"), None);
        assert_eq!(placed.find(1, 1, ""), None);
        let mut changed = sources();
        changed.insert("src/a & b.ts".into(), "export function run() {}\n".into());
        let error = istanbul(&fixture(), Path::new("/w"), &changed).unwrap_err();
        assert!(
            error.starts_with("src/a & b.ts:1 no longer holds"),
            "{error}"
        );
        let error = istanbul(&fixture(), Path::new("/w"), &BTreeMap::new()).unwrap_err();
        assert!(error.contains("could not be read"), "{error}");
    }

    #[test]
    fn istanbul_places_every_statement_function_and_branch() {
        let root = Path::new("/w");
        let written = istanbul(&fixture(), root, &sources()).unwrap();
        let parsed: Value = serde_json::from_str(&written).unwrap();
        let key = absolute(root, "src/a & b.ts");
        let file = &parsed[&key];
        assert_eq!(file["path"], json!(key));
        assert_eq!(
            file["statementMap"],
            json!({
                "0": { "start": { "line": 2, "column": 19 }, "end": { "line": 2, "column": 28 } },
                "1": { "start": { "line": 3, "column": 2 }, "end": { "line": 3, "column": 32 } },
            })
        );
        assert_eq!(file["s"], json!({ "0": 1, "1": 0 }));
        let range =
            json!({ "start": { "line": 1, "column": 0 }, "end": { "line": 4, "column": 1 } });
        assert_eq!(
            file["fnMap"],
            json!({ "0": { "name": "run", "decl": range, "loc": range, "line": 1 } })
        );
        assert_eq!(file["f"], json!({ "0": 1 }));
        let branch =
            json!({ "start": { "line": 2, "column": 2 }, "end": { "line": 2, "column": 28 } });
        assert_eq!(
            file["branchMap"],
            json!({ "0": { "loc": branch, "type": "if", "locations": [branch, branch], "line": 2 } })
        );
        assert_eq!(file["b"], json!({ "0": [1, 0] }));
    }

    #[test]
    fn the_istanbul_summary_states_supercov_s_counts() {
        let root = Path::new("/w");
        let parsed: Value = serde_json::from_str(&istanbul_summary(&fixture(), root)).unwrap();
        let expected = json!({
            "lines": { "total": 4, "covered": 3, "skipped": 0, "pct": 75.0 },
            "statements": { "total": 2, "covered": 1, "skipped": 0, "pct": 50.0 },
            "functions": { "total": 1, "covered": 1, "skipped": 0, "pct": 100.0 },
            "branches": { "total": 2, "covered": 1, "skipped": 0, "pct": 50.0 },
        });
        assert_eq!(parsed["total"], expected);
        assert_eq!(parsed[absolute(root, "src/a & b.ts")], expected);
        assert_eq!(
            istanbul_count(1, 3),
            json!({ "total": 3, "covered": 1, "skipped": 0, "pct": 33.33 })
        );
        assert_eq!(istanbul_count(0, 0)["pct"], json!(100.0));
    }

    #[test]
    fn clover_states_lines_and_the_branches_on_them() {
        let written = clover(&fixture(), 7);
        for expected in [
            "<coverage generated=\"7\" clover=\"3.2.0\">",
            "<metrics statements=\"2\" coveredstatements=\"1\" conditionals=\"2\" coveredconditionals=\"1\" methods=\"1\" coveredmethods=\"1\" elements=\"5\" coveredelements=\"3\" complexity=\"0\" loc=\"4\" ncloc=\"4\" packages=\"1\" files=\"1\" classes=\"1\"/>",
            "<file name=\"a &amp; b.ts\" path=\"src/a &amp; b.ts\">",
            "<line num=\"1\" count=\"1\" type=\"stmt\"/>",
            "<line num=\"2\" count=\"1\" type=\"cond\" truecount=\"1\" falsecount=\"1\"/>",
            "<line num=\"4\" count=\"0\" type=\"stmt\"/>",
        ] {
            assert!(written.contains(expected), "{expected}\n{written}");
        }
    }

    #[test]
    fn jacoco_groups_files_by_directory_and_states_lines_once() {
        let written = jacoco(&fixture(), 7);
        for expected in [
            "<sessioninfo id=\"supercov\" start=\"7000\" dump=\"7000\"/>",
            "<package name=\"src\">",
            "<class name=\"src/a &amp; b\" sourcefilename=\"a &amp; b.ts\">",
            "<method name=\"run\" desc=\"()\" line=\"1\">",
            "<sourcefile name=\"a &amp; b.ts\">",
            "<line nr=\"2\" mi=\"0\" ci=\"1\" mb=\"1\" cb=\"1\"/>",
            "<line nr=\"4\" mi=\"1\" ci=\"0\" mb=\"0\" cb=\"0\"/>",
            "<counter type=\"INSTRUCTION\" missed=\"1\" covered=\"3\"/>",
            "<counter type=\"BRANCH\" missed=\"1\" covered=\"1\"/>",
            "<counter type=\"LINE\" missed=\"1\" covered=\"3\"/>",
            "<counter type=\"METHOD\" missed=\"0\" covered=\"1\"/>",
        ] {
            assert!(written.contains(expected), "{expected}\n{written}");
        }
        // The report's counters close it, after its one package.
        assert!(
            written.ends_with("<counter type=\"METHOD\" missed=\"0\" covered=\"1\"/>\n</report>\n")
        );
    }

    /// A branch on a line no measured line holds is stated on the measured
    /// line above it, so every line format counts every branch.
    #[test]
    fn a_branch_off_a_measured_line_is_stated_where_its_statement_starts() {
        let mut view = fixture();
        let file = &mut view.files[0];
        file.branches.push(BranchRecord {
            line: 5,
            block: 1,
            index: 0,
            taken: false,
        });
        file.branches.push(BranchRecord {
            line: 0,
            block: 2,
            index: 0,
            taken: true,
        });
        assert_eq!(
            branches_by_line(file),
            BTreeMap::from([(1, (1, 0)), (2, (1, 1)), (4, (0, 1))])
        );
        assert!(sonar(&view).contains(
            "<lineToCover lineNumber=\"4\" covered=\"false\" branchesToCover=\"1\" coveredBranches=\"0\"/>"
        ));
        assert!(cobertura(&view, 0).contains(
            "<line number=\"4\" hits=\"0\" branch=\"true\" condition-coverage=\"0% (0/1)\"/>"
        ));
        view.files[0].measured_lines.clear();
        assert_eq!(branches_by_line(&view.files[0]), BTreeMap::new());
    }

    #[test]
    fn sonar_states_each_measured_line_and_its_branches() {
        assert_eq!(
            sonar(&fixture()),
            "<coverage version=\"1\">\n  <file path=\"src/a &amp; b.ts\">\n    <lineToCover lineNumber=\"1\" covered=\"true\"/>\n    <lineToCover lineNumber=\"2\" covered=\"true\" branchesToCover=\"2\" coveredBranches=\"1\"/>\n    <lineToCover lineNumber=\"3\" covered=\"true\"/>\n    <lineToCover lineNumber=\"4\" covered=\"false\"/>\n  </file>\n</coverage>\n"
        );
    }

    #[test]
    fn export_routes_each_format_and_names_them_all_when_asked_for_another() {
        let root = Path::new("/w");
        let view = fixture();
        for format in FORMATS.split('|').filter(|format| *format != "html") {
            assert!(
                export(&view, format, 0, root, &sources()).is_ok(),
                "{format}"
            );
        }
        assert!(needs_source("istanbul"));
        assert!(!needs_source("istanbul-summary"));
        let error = export(&view, "html", 0, root, &sources()).unwrap_err();
        assert!(error.contains(FORMATS), "{error}");
    }

    #[test]
    fn an_lcov_tracefile_states_hit_or_not_hit_and_never_a_frequency() {
        // Supercov records that a line ran, not how often. A consumer displays
        // `DA:` counts as execution frequencies, so writing anything above 1
        // would publish a number no measurement supports.
        let text = lcov(&fixture());
        assert!(text.contains("SF:src/a & b.ts\n"), "{text}");
        assert!(
            text.contains("DA:1,1\n") && text.contains("DA:4,0\n"),
            "{text}"
        );
        assert!(
            text.lines().all(|line| !line.starts_with("DA:")
                || line.ends_with(",1")
                || line.ends_with(",0"))
        );
        assert!(text.contains("FNDA:1,run\n"), "{text}");
        // An untaken branch is `-`, which readers distinguish from zero.
        assert!(
            text.contains("BRDA:2,0,0,1\n") && text.contains("BRDA:2,0,1,-\n"),
            "{text}"
        );
        assert!(text.contains("LF:4\nLH:3\n"), "{text}");
        assert!(text.contains("BRF:2\nBRH:1\n"), "{text}");
        assert!(text.ends_with("end_of_record\n"));
    }

    #[test]
    fn lcov_names_each_condition_and_counts_as_supercov_counts() {
        let text = lcov(&fixture());
        // A decision is a group: its line, its size, then each condition
        // shown turning the outcome both ways or neither.
        assert!(
            text.contains(
                "MCDC:2,2,t,1,0,a\nMCDC:2,2,f,1,0,a\nMCDC:2,2,t,0,1,b == 1\nMCDC:2,2,f,0,1,b == 1\n"
            ),
            "{text}"
        );
        // Two decisions of one size on a line are one group of four, so that
        // no two conditions answer to one name.
        assert!(
            text.contains(
                "MCDC:3,4,t,1,0,c\nMCDC:3,4,f,1,0,c\nMCDC:3,4,t,1,1,d\nMCDC:3,4,f,1,1,d\nMCDC:3,4,t,0,2,e\nMCDC:3,4,f,0,2,e\nMCDC:3,4,t,1,3,f(1, 2)\nMCDC:3,4,f,1,3,f(1, 2)\n"
            ),
            "{text}"
        );
        // Four of six conditions shown is eight of twelve, the same share.
        assert!(text.contains("MCF:12\nMCH:8\n"), "{text}");
        // Every record is one LCOV 2 reads: its own pattern for the line.
        for record in text.lines().filter(|line| line.starts_with("MCDC:")) {
            let fields = record["MCDC:".len()..].splitn(6, ',').collect::<Vec<_>>();
            assert_eq!(fields.len(), 6, "{record}");
            assert!(fields[0].parse::<usize>().is_ok_and(|line| line > 0));
            assert!(fields[1].parse::<usize>().is_ok());
            assert!(matches!(fields[2], "t" | "f"));
            assert!(fields[3].parse::<usize>().is_ok());
            assert!(fields[4].parse::<usize>().unwrap() < fields[1].parse::<usize>().unwrap());
            assert!(!fields[5].is_empty() && !fields[5].contains('\n'));
        }
    }

    #[test]
    fn lcov_records_keep_the_order_readers_parse_them_in() {
        // geninfo defines the sequence, and readers rely on it: a tracefile
        // whose counters precede the records they summarise is accepted by
        // some parsers and silently mis-read by others.
        let text = lcov(&fixture());
        let rank = |line: &str| match line.split(':').next().unwrap_or("") {
            "TN" => 0,
            "SF" => 1,
            "FN" => 2,
            "FNDA" => 3,
            "FNF" => 4,
            "FNH" => 5,
            "BRDA" => 6,
            "BRF" => 7,
            "BRH" => 8,
            "MCDC" => 9,
            "MCF" => 10,
            "MCH" => 11,
            "DA" => 12,
            "LF" => 13,
            "LH" => 14,
            _ => 15,
        };
        for record in text
            .split("end_of_record\n")
            .filter(|r| !r.trim().is_empty())
        {
            let ranks = record
                .trim()
                .lines()
                .map(rank)
                .filter(|rank| *rank < 12)
                .collect::<Vec<_>>();
            let mut sorted = ranks.clone();
            sorted.sort_unstable();
            assert_eq!(ranks, sorted, "out of order:\n{record}");
        }
    }

    #[test]
    fn totals_in_an_export_match_the_run_view_they_came_from() {
        // The whole point of exporting from the shared view: a consumer's
        // percentage has to be the one the gate enforced.
        let view = fixture();
        let text = lcov(&view);
        let found: usize = text
            .lines()
            .filter_map(|line| line.strip_prefix("LF:")?.parse::<usize>().ok())
            .sum();
        let hit: usize = text
            .lines()
            .filter_map(|line| line.strip_prefix("LH:")?.parse::<usize>().ok())
            .sum();
        assert_eq!((hit, found), counts(&view, Metric::Lines));

        let xml = cobertura(&view, 0);
        assert!(
            xml.contains("lines-covered=\"3\" lines-valid=\"4\""),
            "{xml}"
        );
        assert!(
            xml.contains("branches-covered=\"1\" branches-valid=\"2\""),
            "{xml}"
        );
        assert!(xml.contains("line-rate=\"0.7500\""), "{xml}");
    }

    #[test]
    fn cobertura_escapes_markup_and_keeps_paths_relative() {
        // A filename carrying `&` or `<` would otherwise produce a document no
        // reader can parse, and an absolute path would leak this machine's
        // layout into a shared artifact.
        let xml_text = cobertura(&fixture(), 1_700_000_000);
        assert!(
            xml_text.contains("filename=\"src/a &amp; b.ts\""),
            "{xml_text}"
        );
        assert!(!xml_text.contains(" & "), "raw ampersand survived");
        assert!(xml_text.contains("<source>.</source>"));
        assert!(xml_text.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n"));
        // A branching line carries its condition detail; a plain line does not.
        assert!(
            xml_text.contains(
                "<line number=\"2\" hits=\"1\" branch=\"true\" condition-coverage=\"50% (1/2)\"/>"
            ),
            "{xml_text}"
        );
        assert!(
            xml_text.contains("<line number=\"1\" hits=\"1\"/>"),
            "{xml_text}"
        );
        assert_eq!(
            xml("a\u{0}b"),
            "ab",
            "unencodable control bytes are dropped"
        );
    }

    #[test]
    fn xml_keeps_the_whitespace_controls_xml_allows() {
        // Tab, line feed and carriage return are legal XML characters; only
        // the other control characters are unencodable.
        assert_eq!(xml("a\tb\nc\rd\u{1}e"), "a\tb\nc\rde");
    }

    #[test]
    fn a_condition_without_source_is_named_by_its_place_in_the_group() {
        let conditions = [ConditionRecord {
            line: 7,
            column: 1,
            index: 0,
            covered: false,
            source: " \n ".into(),
        }];
        let mut out = String::new();
        assert_eq!(mcdc_records(&conditions, &mut out), (2, 0));
        assert_eq!(
            out,
            "MCDC:7,1,t,0,0,condition 0\nMCDC:7,1,f,0,0,condition 0\n"
        );
    }

    /// A file Supercov knows of but measured nothing in still has its record,
    /// its counters at zero.
    #[test]
    fn a_file_with_nothing_measured_exports_an_empty_record() {
        let mut view = fixture();
        let file = &mut view.files[0];
        // Every metric counted, none with anything eligible.
        file.metrics = Metric::ALL
            .into_iter()
            .map(|each| metric(each, 0, 0))
            .collect();
        file.measured_lines.clear();
        file.uncovered_lines.clear();
        file.functions.clear();
        file.branches.clear();
        file.conditions.clear();
        file.points.clear();
        file.branch_sites.clear();
        let parsed: Value =
            serde_json::from_str(&istanbul(&view, Path::new("/w"), &sources()).unwrap()).unwrap();
        let record = &parsed[absolute(Path::new("/w"), "src/a & b.ts")];
        for map in ["statementMap", "fnMap", "branchMap", "s", "f", "b"] {
            assert_eq!(record[map], json!({}), "{map}");
        }
        let clover_text = clover(&view, 0);
        assert!(
            clover_text.contains("ncloc=\"0\"/>\n    </file>"),
            "{clover_text}"
        );
        let jacoco_text = jacoco(&view, 0);
        assert!(
            jacoco_text.contains("<class name=\"src/a &amp; b\" sourcefilename=\"a &amp; b.ts\">\n      <counter type=\"METHOD\" missed=\"0\" covered=\"0\"/>\n    </class>\n    <sourcefile name=\"a &amp; b.ts\">\n      <counter type=\"INSTRUCTION\""),
            "{jacoco_text}"
        );
        assert_eq!(
            sonar(&view),
            "<coverage version=\"1\">\n  <file path=\"src/a &amp; b.ts\">\n  </file>\n</coverage>\n"
        );
        assert_eq!(
            lcov(&view),
            "TN:\nSF:src/a & b.ts\nFNF:0\nFNH:0\nBRF:0\nBRH:0\nLF:0\nLH:0\nend_of_record\n"
        );
        let xml_text = cobertura(&view, 0);
        assert!(
            xml_text.contains("<methods/>\n          <lines>\n          </lines>"),
            "{xml_text}"
        );
    }

    #[test]
    fn an_empty_denominator_does_not_become_a_zero_rate() {
        // Cobertura has no "not applicable", and reporting 0% for a file with
        // no branches would look like a regression that never happened.
        assert_eq!(rate(0, 0), 1.0);
        let mut view = fixture();
        view.files[0].branches.clear();
        view.files[0].metrics = Metric::ALL
            .into_iter()
            .map(|each| match each {
                Metric::Lines => metric(each, 3, 4),
                _ => metric(each, 0, 0),
            })
            .collect();
        let xml_text = cobertura(&view, 0);
        assert!(xml_text.contains("branch-rate=\"1.0000\""), "{xml_text}");
    }
}
