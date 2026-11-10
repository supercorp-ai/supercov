//! `runs <run> mutants`: each JavaScript mutant the run listed, with how far
//! its tests got toward killing it (`docs/design/mutants.md`), read from the
//! run's index; and, given StrykerJS's own `mutation.json` reports, the two
//! joined by mutant id.
//!
//! The verdicts are worked out when the index is built, from every test the
//! index keeps: an index of some tests (`--tag`, `--kind`) holds their
//! verdicts alone, so a mutant only other tests infect is not infected there.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use supercov_contracts::AgentPagination;

use crate::{
    agent_json,
    coverage_index::{CoverageIndex, CoverageViewId, IndexedMutant, MutantCounts, MutantStatus},
    coverage_query::QueryError,
};

/// What `mutants` was asked beyond a location.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutantsOptions {
    /// `--status`: only mutants with one of these.
    #[serde(default)]
    pub statuses: Vec<MutantStatus>,
    /// `--mutator`: only this StrykerJS mutator's mutants.
    #[serde(default)]
    pub mutator: Option<String>,
    /// `--stryker`: StrykerJS's results to join, read by the caller.
    #[serde(skip)]
    pub stryker: Option<StrykerReports>,
}

impl<'de> Deserialize<'de> for MutantStatus {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let name = String::deserialize(deserializer)?;
        Self::parse(&name)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown mutant status {name}")))
    }
}

/// The StrykerJS reports `--stryker` named, and their mutants.
#[derive(Debug, Clone, Default)]
pub struct StrykerReports {
    /// What `--stryker` was given: reports and directories of them.
    pub given: Vec<String>,
    /// `--stryker-root`, as given.
    pub root: Option<String>,
    /// Every report read, a directory's in order of path.
    pub reports: Vec<String>,
    pub mutants: Vec<StrykerReportMutant>,
}

/// One mutant of a StrykerJS report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrykerReportMutant {
    /// From the project root.
    pub file: String,
    pub start: (u32, u32),
    pub end: (u32, u32),
    pub mutator: String,
    pub replacement: Option<String>,
    /// The mutated code, from the source the report holds.
    pub original: Option<String>,
    pub status: String,
}

/// Read a StrykerJS `mutation.json`: every mutant of every file, its path
/// given from the project root by `path_of`.
pub fn parse_stryker_report(
    json: &str,
    path_of: impl Fn(&str) -> String,
) -> Result<Vec<StrykerReportMutant>, String> {
    let report: Value = serde_json::from_str(json).map_err(|error| error.to_string())?;
    let files = report["files"]
        .as_object()
        .ok_or("it has no `files` object; is it StrykerJS's mutation.json?")?;
    let mut mutants = Vec::new();
    for (path, file) in files {
        let source = file["source"].as_str();
        let listed = file["mutants"]
            .as_array()
            .ok_or_else(|| format!("{path} has no `mutants` list"))?;
        for mutant in listed {
            let position = |at: &str| -> Result<(u32, u32), String> {
                let place = &mutant["location"][at];
                let number = |field: &str| {
                    place[field]
                        .as_u64()
                        .and_then(|value| u32::try_from(value).ok())
                };
                number("line")
                    .zip(number("column"))
                    .ok_or_else(|| format!("a mutant of {path} has no {at} location"))
            };
            let (start, end) = (position("start")?, position("end")?);
            let field = |name: &str| mutant[name].as_str().map(str::to_owned);
            mutants.push(StrykerReportMutant {
                file: path_of(path),
                start,
                end,
                mutator: field("mutatorName")
                    .ok_or_else(|| format!("a mutant of {path} has no mutatorName"))?,
                replacement: field("replacement"),
                original: source.and_then(|source| utf16_slice(source, start, end)),
                status: field("status")
                    .ok_or_else(|| format!("a mutant of {path} has no status"))?,
            });
        }
    }
    Ok(mutants)
}

/// The text between two 1-based lines and 1-based UTF-16 columns, with
/// lines ended as JavaScript ends them.
fn utf16_slice(source: &str, start: (u32, u32), end: (u32, u32)) -> Option<String> {
    let mut lines = vec![0_usize];
    let mut characters = source.char_indices().peekable();
    while let Some((at, character)) = characters.next() {
        match character {
            '\r' if characters.peek().is_some_and(|(_, next)| *next == '\n') => {}
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => lines.push(at + character.len_utf8()),
            _ => {}
        }
    }
    let offset = |(line, column): (u32, u32)| -> Option<usize> {
        let from = *lines.get((line as usize).checked_sub(1)?)?;
        let mut units = 0_u32;
        for (at, character) in source[from..].char_indices() {
            if units + 1 >= column {
                return Some(from + at);
            }
            units += character.len_utf16() as u32;
        }
        (units + 1 == column).then_some(source.len())
    };
    source.get(offset(start)?..offset(end)?).map(str::to_owned)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct MutantLevels {
    pub weak: bool,
    pub strong: bool,
}

/// Counts and scores of some mutants.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutantsSummary {
    pub mutants: usize,
    pub status: MutantCounts,
    /// `infected / (infected + not-infected + not-reached)`; unknown
    /// mutants are left out.
    pub weak_score: Option<f64>,
    /// `infected / (infected + not-infected)`: over reached code.
    pub weak_score_reached: Option<f64>,
    /// Reserved for strong mutation; never estimated from weak results.
    pub mutation_score: Option<f64>,
}

impl MutantsSummary {
    pub fn of(status: MutantCounts) -> Self {
        let score = |denominator: usize| {
            (denominator > 0).then(|| {
                (status.infected as f64 / denominator as f64 * 10_000.0).round() / 10_000.0
            })
        };
        Self {
            mutants: status.total(),
            weak_score: score(status.infected + status.not_infected + status.not_reached),
            weak_score_reached: score(status.infected + status.not_infected),
            mutation_score: None,
            status,
        }
    }
}

/// The run summary's line: its mutants and scores.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutantsRunSummary {
    pub mutant_set: String,
    #[serde(flatten)]
    pub summary: MutantsSummary,
}

/// The run's mutants in one line of its summary, when it listed any.
pub fn run_summary(index: &CoverageIndex<'_>) -> Result<Option<MutantsRunSummary>, QueryError> {
    let Some(mutation) = index.mutation()? else {
        return Ok(None);
    };
    let mut counts = MutantCounts::default();
    mutation
        .files
        .iter()
        .for_each(|file| counts.merge(&file.counts));
    Ok((counts.total() > 0).then(|| MutantsRunSummary {
        mutant_set: mutation.mutant_set,
        summary: MutantsSummary::of(counts),
    }))
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MutantSpan {
    pub start: [u32; 2],
    pub end: [u32; 2],
}

/// The steps weak mutation answered for a mutant.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WeakResult {
    /// `reached`, `not-reached`, or `unknown`.
    pub reach: &'static str,
    /// `infected`, `not-infected`, `unknown`, or null where not reached.
    pub infection: Option<&'static str>,
    /// A stable code for the reason.
    pub code: String,
    pub reason: String,
    /// An infection that is only that the code around it ran.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub proxy: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutantEntry {
    pub id: String,
    pub file: String,
    pub span: MutantSpan,
    pub mutator: String,
    pub original: String,
    pub replacement: String,
    pub status: MutantStatus,
    pub weak: WeakResult,
    /// Reserved for strong mutation: `{ status, tests, durationMs }`.
    pub strong: Option<Value>,
    /// StrykerJS's status, joined by id, when `--stryker` was given.
    pub stryker: Option<String>,
}

/// Where no statement or function point contains the mutant, the run
/// cannot say whether it ran.
const UNPLACED: [&str; 2] = ["no-statement-point", "no-point"];

impl MutantEntry {
    fn of(mutant: &IndexedMutant, stryker: Option<String>) -> Self {
        let reach = match mutant.status {
            MutantStatus::NotReached => "not-reached",
            MutantStatus::Unknown if UNPLACED.contains(&mutant.code.as_str()) => "unknown",
            _ => "reached",
        };
        Self {
            id: mutant.id(),
            file: mutant.file.clone(),
            span: MutantSpan {
                start: [mutant.start.0, mutant.start.1],
                end: [mutant.end.0, mutant.end.1],
            },
            mutator: mutant.mutator.clone(),
            original: mutant.original.clone(),
            replacement: mutant.replacement.clone(),
            status: mutant.status,
            weak: WeakResult {
                reach,
                infection: (mutant.status != MutantStatus::NotReached)
                    .then(|| mutant.status.name()),
                code: mutant.code.clone(),
                reason: mutant.reason.clone(),
                proxy: mutant.proxy,
            },
            strong: None,
            stryker,
        }
    }
}

/// One file of the per-file table: its mutants by status.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutantFileRow {
    pub file: String,
    pub status: MutantCounts,
}

impl MutantFileRow {
    /// No test ran any of the file's mutants.
    pub fn unreached(&self) -> bool {
        self.status.total() > 0 && self.status.not_reached == self.status.total()
    }
}

/// The per-file table's order. First what only this report shows: the
/// mutants tests reach and do not infect. Then what no test reaches, which
/// coverage shows too; the files no test ran at all last, however many
/// mutants they hold, and by name where counts tie.
fn table_order(rows: &mut [MutantFileRow]) {
    rows.sort_by(|left, right| {
        right
            .status
            .not_infected
            .cmp(&left.status.not_infected)
            .then_with(|| left.unreached().cmp(&right.unreached()))
            .then_with(|| right.status.not_reached.cmp(&left.status.not_reached))
            .then_with(|| left.file.cmp(&right.file))
    });
}

/// Mutants by mutator.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MutatorCounts {
    pub total: usize,
    pub by_mutator: BTreeMap<String, usize>,
}

impl MutatorCounts {
    fn add(&mut self, mutator: &str) {
        self.total += 1;
        *self.by_mutator.entry(mutator.to_owned()).or_default() += 1;
    }
}

/// A mutant this run says no test infects or reaches, which StrykerJS
/// killed: weak mutation said it must survive, and it did not.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundnessViolation {
    pub id: String,
    pub status: MutantStatus,
    pub stryker: String,
    pub reason: String,
}

/// How many violations are listed; the total is always given.
const LISTED_VIOLATIONS: usize = 50;

#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SoundnessViolations {
    pub total: usize,
    pub mutants: Vec<SoundnessViolation>,
}

/// StrykerJS's survivors (Survived, NoCoverage) among the matched mutants,
/// and how many of them this run says no test infects or reaches.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SurvivorsExplained {
    pub survivors: usize,
    pub explained: usize,
}

/// The join with StrykerJS's reports.
#[derive(Debug, Clone, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrykerJoin {
    pub reports: Vec<String>,
    pub matched: usize,
    pub only_supercov: MutatorCounts,
    pub only_stryker: MutatorCounts,
    pub soundness_violations: SoundnessViolations,
    pub survivors_explained: SurvivorsExplained,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageMutantsData {
    pub run: String,
    pub mutant_set: Option<String>,
    pub levels: MutantLevels,
    /// The file and line the summary and list are of, when one was asked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Of the mutants asked about (a file, a line, a mutator), before
    /// `--status` picks some.
    pub summary: MutantsSummary,
    /// Why there are none, and how to get them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub mutants: Vec<MutantEntry>,
    pub stryker: Option<StrykerJoin>,
    /// Every file's counts, for the table a reader is shown without a file.
    #[serde(skip)]
    pub files: Vec<MutantFileRow>,
}

/// StrykerJS's statuses by how far toward a kill they got; a status not
/// here is no result (Ignored, CompileError, RuntimeError, Pending).
fn stryker_rank(status: &str) -> Option<u8> {
    match status {
        "Killed" | "Timeout" => Some(3),
        "Survived" => Some(2),
        "NoCoverage" => Some(1),
        _ => None,
    }
}

/// A mutant's place without its descriptor: file, start, end and mutator.
type Place<'a> = (&'a str, (u32, u32), (u32, u32), &'a str);

/// Each StrykerJS mutant by Supercov's id, its several reports merged by
/// rank, with its mutator.
fn stryker_by_id(
    reports: &[StrykerReportMutant],
    originals: &HashMap<Place<'_>, &str>,
) -> BTreeMap<String, (String, String)> {
    let mut merged = BTreeMap::<String, (String, String)>::new();
    for mutant in reports {
        let Some(rank) = stryker_rank(&mutant.status) else {
            continue;
        };
        let original = mutant.original.as_deref().or_else(|| {
            originals
                .get(&(
                    mutant.file.as_str(),
                    mutant.start,
                    mutant.end,
                    mutant.mutator.as_str(),
                ))
                .copied()
        });
        let replacement = mutant.replacement.as_deref().unwrap_or_default();
        let descriptor = original
            .and_then(|original| {
                crate::js_mutants::descriptor_from_stryker(&mutant.mutator, original, replacement)
            })
            .unwrap_or_else(|| format!("?{replacement}"));
        let id = format!(
            "{}:{}:{}-{}:{}:{}:{descriptor}",
            mutant.file, mutant.start.0, mutant.start.1, mutant.end.0, mutant.end.1, mutant.mutator
        );
        let kept = merged
            .get(&id)
            .and_then(|(_, status)| stryker_rank(status))
            .is_some_and(|kept| kept >= rank);
        if !kept {
            merged.insert(id, (mutant.mutator.clone(), mutant.status.clone()));
        }
    }
    merged
}

fn unkilled(status: MutantStatus) -> bool {
    matches!(status, MutantStatus::NotInfected | MutantStatus::NotReached)
}

fn join(
    asked: &[IndexedMutant],
    reports: &StrykerReports,
    in_scope: impl Fn(&StrykerReportMutant) -> bool,
) -> (StrykerJoin, BTreeMap<String, (String, String)>) {
    let originals = asked
        .iter()
        .map(|mutant| {
            (
                (
                    mutant.file.as_str(),
                    mutant.start,
                    mutant.end,
                    mutant.mutator.as_str(),
                ),
                mutant.original.as_str(),
            )
        })
        .collect::<HashMap<_, _>>();
    let scoped = reports
        .mutants
        .iter()
        .filter(|mutant| in_scope(mutant))
        .cloned()
        .collect::<Vec<_>>();
    let stryker = stryker_by_id(&scoped, &originals);
    let mut joined = StrykerJoin {
        reports: reports.reports.clone(),
        ..StrykerJoin::default()
    };
    let mut seen = std::collections::HashSet::new();
    for mutant in asked {
        let id = mutant.id();
        let Some((_, status)) = stryker.get(&id) else {
            joined.only_supercov.add(&mutant.mutator);
            continue;
        };
        seen.insert(id.clone());
        joined.matched += 1;
        if stryker_rank(status) == Some(3) && unkilled(mutant.status) {
            joined.soundness_violations.total += 1;
            if joined.soundness_violations.mutants.len() < LISTED_VIOLATIONS {
                joined
                    .soundness_violations
                    .mutants
                    .push(SoundnessViolation {
                        id,
                        status: mutant.status,
                        stryker: status.clone(),
                        reason: mutant.reason.clone(),
                    });
            }
        } else if stryker_rank(status).is_some_and(|rank| rank < 3) {
            joined.survivors_explained.survivors += 1;
            if unkilled(mutant.status) {
                joined.survivors_explained.explained += 1;
            }
        }
    }
    for (id, (mutator, _)) in &stryker {
        if !seen.contains(id) {
            joined.only_stryker.add(mutator);
        }
    }
    (joined, stryker)
}

/// What `mutants` was asked.
pub struct CoverageMutantsQueryOptions<'a> {
    pub run: &'a str,
    pub file: Option<&'a str>,
    pub line: Option<usize>,
    pub options: &'a MutantsOptions,
    pub offset: usize,
    pub limit: usize,
}

const NO_LIST: &str = "This run did not list its mutants. Mutants are listed for JavaScript and TypeScript when the run's command is measured with SUPERCOV_COMPARISONS=1 or SUPERCOV_INFECTION_PROBES=1; measure it again with one of them set.";

pub fn coverage_mutants_query(
    index: &CoverageIndex<'_>,
    options: CoverageMutantsQueryOptions<'_>,
) -> Result<(CoverageMutantsData, AgentPagination), QueryError> {
    if options.limit == 0 {
        return Err(QueryError::InvalidPagination);
    }
    let mutation = index.mutation()?;
    let files = mutation
        .as_ref()
        .map_or(&[][..], |mutation| mutation.files.as_slice());
    let mut note = None;
    let selected = match options.file {
        None => files.iter().collect::<Vec<_>>(),
        Some(selector) => {
            let exact = files.iter().find(|file| file.file == selector);
            let matches = files
                .iter()
                .filter(|file| file.file.contains(selector))
                .collect::<Vec<_>>();
            match exact {
                Some(file) => vec![file],
                None if matches.len() == 1 => matches,
                None if matches.is_empty() => {
                    let included =
                        index
                            .scope_entries(CoverageViewId::All)?
                            .into_iter()
                            .any(|entry| {
                                entry.file == selector
                                    && entry.status.eq_ignore_ascii_case("included")
                            });
                    if !included || mutation.is_none() {
                        return Err(QueryError::SourceNotFound(selector.into()));
                    }
                    note = Some(format!("{selector} has no mutants in this run."));
                    Vec::new()
                }
                None => {
                    return Err(QueryError::AmbiguousSelector {
                        selector: selector.into(),
                        matches: matches.iter().map(|file| file.file.clone()).collect(),
                    });
                }
            }
        }
    };
    let file = options.file.map(|selector| {
        selected
            .first()
            .map_or(selector, |file| file.file.as_str())
            .to_owned()
    });
    let mutator = options.options.mutator.as_deref();
    let line = options
        .line
        .map(|line| u32::try_from(line).unwrap_or(u32::MAX));
    // The mutants asked about, before `--status` picks some of them.
    let mut asked = Vec::new();
    for file in &selected {
        for mutant in index.mutants_of(file)? {
            if line.is_some_and(|line| mutant.start.0 != line)
                || mutator.is_some_and(|mutator| !mutant.mutator.eq_ignore_ascii_case(mutator))
            {
                continue;
            }
            asked.push(mutant);
        }
    }
    let mut counts = MutantCounts::default();
    asked.iter().for_each(|mutant| counts.add(mutant.status));
    let (stryker, statuses) = match &options.options.stryker {
        Some(reports) => {
            let in_scope = |mutant: &StrykerReportMutant| {
                file.as_deref().is_none_or(|file| mutant.file == file)
                    && line.is_none_or(|line| mutant.start.0 == line)
                    && mutator.is_none_or(|mutator| mutant.mutator.eq_ignore_ascii_case(mutator))
            };
            let (joined, statuses) = join(&asked, reports, in_scope);
            (Some(joined), statuses)
        }
        None => (None, BTreeMap::new()),
    };
    let wanted = &options.options.statuses;
    let listed = asked
        .iter()
        .filter(|mutant| wanted.is_empty() || wanted.contains(&mutant.status))
        .collect::<Vec<_>>();
    let total = listed.len();
    let page = listed
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .map(|mutant| {
            let stryker = statuses.get(&mutant.id()).map(|(_, status)| status.clone());
            MutantEntry::of(mutant, stryker)
        })
        .collect::<Vec<_>>();
    let pagination = agent_json::pagination(options.offset, options.limit, page.len(), total);
    let mut rows = files
        .iter()
        .map(|file| MutantFileRow {
            file: file.file.clone(),
            status: file.counts.clone(),
        })
        .collect::<Vec<_>>();
    table_order(&mut rows);
    if mutation.is_none() {
        note = Some(NO_LIST.into());
    } else if files.is_empty() {
        note = Some("The run listed its mutants, and the files it measured have none.".into());
    }
    Ok((
        CoverageMutantsData {
            run: options.run.into(),
            mutant_set: mutation
                .as_ref()
                .map(|mutation| mutation.mutant_set.clone()),
            levels: MutantLevels {
                weak: mutation.is_some(),
                strong: false,
            },
            file,
            line,
            summary: MutantsSummary::of(counts),
            note,
            mutants: page,
            stryker,
            files: rows,
        },
        pagination,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_s_columns_are_utf16_and_its_lines_end_as_javascript_ends_them() {
        let source = "let a = 1;\r\nconst é = '😀' + b;\n  x?.y";
        assert_eq!(
            utf16_slice(source, (1, 5), (1, 10)).as_deref(),
            Some("a = 1")
        );
        assert_eq!(
            utf16_slice(source, (2, 11), (2, 19)).as_deref(),
            Some("'😀' + b")
        );
        assert_eq!(utf16_slice(source, (3, 3), (3, 7)).as_deref(), Some("x?.y"));
        assert_eq!(utf16_slice(source, (4, 1), (4, 2)), None);
    }

    #[test]
    fn several_reports_keep_each_mutant_s_furthest_status() {
        let mutant = |status: &str| StrykerReportMutant {
            file: "src/a.js".into(),
            start: (1, 5),
            end: (1, 10),
            mutator: "EqualityOperator".into(),
            replacement: Some("a <= b".into()),
            original: Some("a < b".into()),
            status: status.into(),
        };
        let merged = stryker_by_id(
            &[
                mutant("NoCoverage"),
                mutant("Killed"),
                mutant("Survived"),
                mutant("Ignored"),
            ],
            &HashMap::new(),
        );
        assert_eq!(
            merged.into_iter().collect::<Vec<_>>(),
            [(
                "src/a.js:1:5-1:10:EqualityOperator:<=".to_owned(),
                ("EqualityOperator".to_owned(), "Killed".to_owned())
            )]
        );
        let dropped = stryker_by_id(&[mutant("CompileError")], &HashMap::new());
        assert!(dropped.is_empty());
    }

    #[test]
    fn the_table_puts_mutants_not_infected_first_and_files_no_test_ran_last() {
        let row =
            |file: &str, not_infected: usize, not_reached: usize, infected: usize| MutantFileRow {
                file: file.into(),
                status: MutantCounts {
                    not_reached,
                    not_infected,
                    infected,
                    unknown: 0,
                },
            };
        let mut rows = vec![
            row("never-loaded.tsx", 0, 1475, 0),
            row("few-reached.ts", 0, 12, 3),
            row("b-not-infected.ts", 4, 0, 10),
            row("a-not-infected.ts", 4, 0, 10),
            row("most-not-infected.ts", 9, 300, 1),
            row("never-loaded-small.ts", 0, 3, 0),
            row("all-infected.ts", 0, 0, 7),
        ];
        table_order(&mut rows);
        assert_eq!(
            rows.iter().map(|row| row.file.as_str()).collect::<Vec<_>>(),
            [
                "most-not-infected.ts",
                "a-not-infected.ts",
                "b-not-infected.ts",
                "few-reached.ts",
                "all-infected.ts",
                "never-loaded.tsx",
                "never-loaded-small.ts",
            ]
        );
    }

    #[test]
    fn scores_leave_unknown_mutants_out_and_are_null_over_nothing() {
        let summary = MutantsSummary::of(MutantCounts {
            not_reached: 1,
            not_infected: 1,
            infected: 2,
            unknown: 5,
        });
        assert_eq!(summary.mutants, 9);
        assert_eq!(summary.weak_score, Some(0.5));
        assert_eq!(summary.weak_score_reached, Some(0.6667));
        assert_eq!(summary.mutation_score, None);
        let none = MutantsSummary::of(MutantCounts {
            unknown: 3,
            ..MutantCounts::default()
        });
        assert_eq!((none.weak_score, none.weak_score_reached), (None, None));
    }
}
