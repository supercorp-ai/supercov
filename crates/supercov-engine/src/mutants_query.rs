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
    coverage_index::{
        CoverageIndex, CoverageViewId, IndexedMutant, MutantCounts, MutantStatus, Untestable,
    },
    coverage_query::QueryError,
};

/// What `mutants` was asked beyond a location.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutantsOptions {
    /// `--status`: only mutants with one of these.
    #[serde(default)]
    pub statuses: Vec<MutantStatus>,
    /// `--status` with strong verdicts (`survived`, `killed`, ...): only
    /// mutants a strong pass gave one of these.
    #[serde(default)]
    pub strong_statuses: Vec<String>,
    /// `--mutator`: only this StrykerJS mutator's mutants.
    #[serde(default)]
    pub mutator: Option<String>,
    /// `--stryker`: StrykerJS's results to join, read by the caller.
    #[serde(skip)]
    pub stryker: Option<StrykerReports>,
    /// `--diff <base>`: the ref the caller reads the change against.
    #[serde(default)]
    pub diff_base: Option<String>,
    /// `--diff`: only the mutants on lines a change added or modified,
    /// worked out by the caller from git.
    #[serde(skip)]
    pub diff: Option<MutantDiff>,
    /// `--fail-on`: the statuses no mutant asked about may have, for a gate.
    #[serde(default)]
    pub fail_on: Vec<MutantStatus>,
    /// The strong verdicts `mutants test` saved into the run, by mutant id,
    /// read by the caller: each `{ status, killedBy, tests, durationMs }`.
    #[serde(skip)]
    pub strong: Option<BTreeMap<String, Value>>,
}

/// `--fail-on`: how many of the mutants asked about have a status the gate
/// refuses.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailOn {
    pub statuses: Vec<MutantStatus>,
    pub mutants: usize,
}

/// The lines a change added or modified, by file, as `--diff <base>` reads
/// them from git, and what they were read against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MutantDiff {
    /// The ref asked for (`origin/main`).
    pub base: String,
    /// Where the change left it.
    pub merge_base: String,
    /// The commit the run measured, compared with the merge base; `None`
    /// where the working tree was, as for a run with uncommitted changes.
    pub commit: Option<String>,
    /// Added and modified lines, 1-based, by file as the run names it.
    pub changed: BTreeMap<String, std::collections::BTreeSet<u32>>,
    /// Files the change added whole that git does not track yet: every line.
    pub untracked: std::collections::BTreeSet<String>,
}

impl MutantDiff {
    /// Whether a mutant spanning lines `start..=end` of `file` is on a line
    /// the change added or modified.
    pub fn touches(&self, file: &str, start: u32, end: u32) -> bool {
        self.untracked.contains(file)
            || self
                .changed
                .get(file)
                .is_some_and(|lines| lines.range(start..=end).next().is_some())
    }
}

/// What `--diff` read, in the reply.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffSummary {
    pub base: String,
    pub merge_base: String,
    /// The commit compared with the merge base, or null for the working
    /// tree.
    pub commit: Option<String>,
    /// Added or modified lines in the files the run measured.
    pub changed_lines: usize,
    /// Of those, the lines a mutant of the files asked about is on.
    pub lines_with_mutants: usize,
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
    /// Supercov's id for it, where the report was matched to Supercov's
    /// mutants by what each mutant makes of the program (the mutant gem's
    /// session results) rather than by span and replacement.
    pub id: Option<String>,
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
                id: None,
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
    /// Of the not-infected mutants, those at a clock or random boundary:
    /// practically untestable, and still counted as not infected.
    pub untestable: usize,
    /// `infected / (infected + not-infected + not-reached)`; unknown and
    /// equivalent mutants are left out.
    pub weak_score: Option<f64>,
    /// `infected / (infected + not-infected)`: over reached code.
    pub weak_score_reached: Option<f64>,
    /// The strong verdicts of these mutants, StrykerJS's score over them;
    /// never estimated from weak results.
    pub mutation_score: Option<f64>,
    /// How many of these mutants a strong pass gave each verdict, where one
    /// ran.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strong: Option<BTreeMap<String, usize>>,
}

impl MutantsSummary {
    pub fn of(status: MutantCounts, untestable: usize) -> Self {
        let score = |denominator: usize| {
            (denominator > 0).then(|| {
                (status.infected as f64 / denominator as f64 * 10_000.0).round() / 10_000.0
            })
        };
        Self {
            mutants: status.total(),
            untestable,
            weak_score: score(status.infected + status.not_infected + status.not_reached),
            weak_score_reached: score(status.infected + status.not_infected),
            mutation_score: None,
            strong: None,
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
    let untestable = mutation.files.iter().map(|file| file.untestable).sum();
    Ok((counts.total() > 0).then(|| MutantsRunSummary {
        mutant_set: mutation.mutant_set,
        summary: MutantsSummary::of(counts, untestable),
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
    /// `reached`, `not-reached`, or `unknown` (an equivalent mutant's,
    /// which is decided before reach).
    pub reach: &'static str,
    /// `infected`, `not-infected`, `unknown`, or null where not reached or
    /// equivalent.
    pub infection: Option<&'static str>,
    /// A stable code for the reason.
    pub code: String,
    pub reason: String,
    /// An infection that is only that the code around it ran.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub proxy: bool,
    /// The static rule an equivalent mutant is shown equivalent by.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// `clock` or `random`: the input that would make it differ is one
    /// exact reading, practically out of a test's reach. A hint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untestable: Option<Untestable>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untestable_reason: Option<String>,
}

/// The part of the code a mutant changes, and what replaces it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MutantChange {
    pub from: String,
    pub to: String,
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
    /// Where `original` is on several lines or is too long to show beside
    /// its replacement: the part the mutant changes, without the start and
    /// end they share, which a reader is shown instead.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<MutantChange>,
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

/// How many characters `original → replacement` may take to be shown whole,
/// as the per-file listing shows a mutant.
pub const SHOWN_WHOLE: usize = 60;

impl MutantEntry {
    fn of(mutant: &IndexedMutant, stryker: Option<String>) -> Self {
        let reach = match mutant.status {
            MutantStatus::NotReached => "not-reached",
            MutantStatus::Unknown if UNPLACED.contains(&mutant.code.as_str()) => "unknown",
            MutantStatus::Equivalent => "unknown",
            _ => "reached",
        };
        let whole = mutant.original.chars().count() + 3 + mutant.replacement.chars().count();
        let shown_whole = mutant.start.0 == mutant.end.0 && whole <= SHOWN_WHOLE;
        let changed = mutant
            .changed
            .as_ref()
            .filter(|_| !shown_whole)
            .map(|(from, to)| MutantChange {
                from: from.clone(),
                to: to.clone(),
            });
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
            changed,
            status: mutant.status,
            weak: WeakResult {
                reach,
                infection: (!matches!(
                    mutant.status,
                    MutantStatus::NotReached | MutantStatus::Equivalent
                ))
                .then(|| mutant.status.name()),
                code: mutant.code.clone(),
                reason: mutant.reason.clone(),
                proxy: mutant.proxy,
                rule: (mutant.status == MutantStatus::Equivalent).then(|| mutant.code.clone()),
                untestable: mutant.untestable.as_ref().map(|(kind, _)| *kind),
                untestable_reason: mutant.untestable.as_ref().map(|(_, reason)| reason.clone()),
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
    /// No test ran any of the file's mutants (an equivalent one needs none).
    pub fn unreached(&self) -> bool {
        self.status.not_reached > 0
            && self.status.not_reached + self.status.equivalent == self.status.total()
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
    /// With `--diff`: what the change was read against. Its mutants are
    /// then listed not infected first, then not reached, then the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<DiffSummary>,
    /// With `--fail-on`: the gate's count, which fails the command when
    /// any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fail_on: Option<FailOn>,
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
        if let Some(id) = &mutant.id {
            let kept = merged
                .get(id)
                .and_then(|(_, status)| stryker_rank(status))
                .is_some_and(|kept| kept >= rank);
            if !kept {
                merged.insert(id.clone(), (mutant.mutator.clone(), mutant.status.clone()));
            }
            continue;
        }
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

/// A status that says StrykerJS cannot kill the mutant. An equivalent one
/// it killed is a static rule's violation.
fn unkilled(status: MutantStatus) -> bool {
    matches!(
        status,
        MutantStatus::NotInfected | MutantStatus::NotReached | MutantStatus::Equivalent
    )
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

const NO_LIST: &str = "This run has no mutants listed. A JavaScript or TypeScript run lists them from its sources the first time they are asked for, while the sources are as the run measured them; a run of another language has none. supercov mutants measures the checkout again where it changed, and tests its mutants.";

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
    let diff = options.options.diff.as_ref();
    let mut diff_summary = None;
    if let Some(diff) = diff {
        asked.retain(|mutant| diff.touches(&mutant.file, mutant.start.0, mutant.end.0));
        let measured = files
            .iter()
            .map(|file| file.file.as_str())
            .collect::<std::collections::HashSet<_>>();
        let mut changed_lines = 0;
        let mut lines_with_mutants = 0;
        for (file, lines) in &diff.changed {
            if !measured.contains(file.as_str()) {
                continue;
            }
            changed_lines += lines.len();
            lines_with_mutants += lines
                .iter()
                .filter(|line| {
                    asked.iter().any(|mutant| {
                        &mutant.file == file && mutant.start.0 <= **line && **line <= mutant.end.0
                    })
                })
                .count();
        }
        for file in &diff.untracked {
            let mut lines = std::collections::BTreeSet::new();
            for mutant in asked.iter().filter(|mutant| &mutant.file == file) {
                lines.extend(mutant.start.0..=mutant.end.0);
            }
            lines_with_mutants += lines.len();
            changed_lines += lines.len();
        }
        diff_summary = Some(DiffSummary {
            base: diff.base.clone(),
            merge_base: diff.merge_base.clone(),
            commit: diff.commit.clone(),
            changed_lines,
            lines_with_mutants,
        });
    }
    let mut counts = MutantCounts::default();
    asked.iter().for_each(|mutant| counts.add(mutant.status));
    let untestable = asked
        .iter()
        .filter(|mutant| mutant.status == MutantStatus::NotInfected && mutant.untestable.is_some())
        .count();
    let (stryker, statuses) = match &options.options.stryker {
        Some(reports) => {
            let in_scope = |mutant: &StrykerReportMutant| {
                file.as_deref().is_none_or(|file| mutant.file == file)
                    && line.is_none_or(|line| mutant.start.0 == line)
                    && diff
                        .is_none_or(|diff| diff.touches(&mutant.file, mutant.start.0, mutant.end.0))
                    && mutator.is_none_or(|mutator| mutant.mutator.eq_ignore_ascii_case(mutator))
            };
            let (joined, statuses) = join(&asked, reports, in_scope);
            (Some(joined), statuses)
        }
        None => (None, BTreeMap::new()),
    };
    // StrykerJS's score over the asked mutants a strong pass gave a verdict:
    // killed and timed out over those and the survivors and uncovered.
    let strong_score = options.options.strong.as_ref().and_then(|strong| {
        let (mut detected, mut total) = (0usize, 0usize);
        for mutant in &asked {
            match strong
                .get(&mutant.id())
                .and_then(|verdict| verdict["status"].as_str())
            {
                Some("killed" | "timeout") => {
                    detected += 1;
                    total += 1;
                }
                Some("survived" | "no-coverage") => total += 1,
                _ => {}
            }
        }
        (total > 0).then(|| (detected as f64 / total as f64 * 10_000.0).round() / 10_000.0)
    });
    let gate = &options.options.fail_on;
    let fail_on = (!gate.is_empty()).then(|| FailOn {
        statuses: gate.clone(),
        mutants: gate.iter().map(|status| counts.of(*status)).sum(),
    });
    let strong_of = |mutant: &IndexedMutant| {
        options
            .options
            .strong
            .as_ref()
            .and_then(|strong| strong.get(&mutant.id()))
            .and_then(|verdict| verdict["status"].as_str().map(str::to_owned))
    };
    let strong_counts = options.options.strong.as_ref().map(|_| {
        let mut counts = BTreeMap::<String, usize>::new();
        for status in asked.iter().filter_map(&strong_of) {
            *counts.entry(status).or_default() += 1;
        }
        counts
    });
    let wanted = &options.options.statuses;
    let wanted_strong = &options.options.strong_statuses;
    let mut listed = asked
        .iter()
        .filter(|mutant| wanted.is_empty() || wanted.contains(&mutant.status))
        .filter(|mutant| {
            wanted_strong.is_empty()
                || strong_of(mutant).is_some_and(|status| wanted_strong.contains(&status))
        })
        .collect::<Vec<_>>();
    // A change's mutants, what its tests leave alive first: a stable sort
    // keeps them by file and position within each status.
    if diff.is_some() {
        listed.sort_by_key(|mutant| match mutant.status {
            MutantStatus::NotInfected => 0,
            MutantStatus::NotReached => 1,
            _ => 2,
        });
    }
    let total = listed.len();
    let page = listed
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .map(|mutant| {
            let stryker = statuses.get(&mutant.id()).map(|(_, status)| status.clone());
            let mut entry = MutantEntry::of(mutant, stryker);
            entry.strong = options
                .options
                .strong
                .as_ref()
                .and_then(|strong| strong.get(&entry.id).cloned());
            entry
        })
        .collect::<Vec<_>>();
    let pagination = agent_json::pagination(options.offset, options.limit, page.len(), total);
    let mut rows = match diff {
        // The change's mutants of each file it touched.
        Some(_) => {
            let mut by_file = BTreeMap::<&str, MutantCounts>::new();
            for mutant in &asked {
                by_file.entry(&mutant.file).or_default().add(mutant.status);
            }
            by_file
                .into_iter()
                .map(|(file, status)| MutantFileRow {
                    file: file.to_owned(),
                    status,
                })
                .collect::<Vec<_>>()
        }
        None => files
            .iter()
            .map(|file| MutantFileRow {
                file: file.file.clone(),
                status: file.counts.clone(),
            })
            .collect::<Vec<_>>(),
    };
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
                strong: strong_score.is_some(),
            },
            file,
            line,
            summary: MutantsSummary {
                mutation_score: strong_score,
                strong: strong_counts.filter(|counts| !counts.is_empty()),
                ..MutantsSummary::of(counts, untestable)
            },
            note,
            mutants: page,
            stryker,
            files: rows,
            diff: diff_summary,
            fail_on,
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
            id: None,
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
                    equivalent: 0,
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

    fn indexed(
        start: (u32, u32),
        end: (u32, u32),
        original: &str,
        replacement: &str,
        changed: Option<(&str, &str)>,
    ) -> IndexedMutant {
        IndexedMutant {
            file: "app/route.ts".into(),
            start,
            end,
            mutator: "MethodExpression".into(),
            descriptor: "removed".into(),
            original: original.into(),
            replacement: replacement.into(),
            changed: changed.map(|(from, to)| (from.into(), to.into())),
            status: MutantStatus::NotInfected,
            proxy: false,
            code: "c".into(),
            reason: "r".into(),
            untestable: None,
        }
    }

    /// A mutant on several lines, or too long to show beside its
    /// replacement, is shown by the part it changes; a short one whole.
    #[test]
    fn a_long_mutant_is_shown_by_the_part_it_changes() {
        let slice = MutantEntry::of(
            &indexed(
                (687, 7),
                (689, 25),
                "candidates .filter((c) => c.ready) .slice(0, maxUsers)",
                "candidates .filter((c) => c.ready)",
                Some(("….slice(0, maxUsers)", "…")),
            ),
            None,
        );
        let filter = MutantEntry::of(
            &indexed(
                (687, 7),
                (688, 29),
                "candidates .filter((c) => c.ready)",
                "candidates",
                Some(("….filter((c) => c.ready)", "…")),
            ),
            None,
        );
        assert_eq!(
            slice.changed,
            Some(MutantChange {
                from: "….slice(0, maxUsers)".into(),
                to: "…".into()
            })
        );
        // Two mutants of one chain start alike and are told apart.
        assert_ne!(slice.changed, filter.changed);
        // The original and replacement stay as they were.
        assert_eq!(
            slice.original,
            "candidates .filter((c) => c.ready) .slice(0, maxUsers)"
        );
        let json = serde_json::to_value(&slice).unwrap();
        assert_eq!(
            json["changed"],
            serde_json::json!({"from": "….slice(0, maxUsers)", "to": "…"})
        );
        // One line too long to show whole.
        let long = MutantEntry::of(
            &indexed(
                (3, 1),
                (3, 70),
                "accounts.filter((account) => account.active).map(toRow).slice(0, n)",
                "accounts.filter((account) => account.active).map(toRow)",
                Some(("….slice(0, n)", "…")),
            ),
            None,
        );
        assert!(long.changed.is_some());
        // A short one fits, and is shown whole.
        let boundary = MutantEntry::of(
            &IndexedMutant {
                mutator: "EqualityOperator".into(),
                descriptor: "<=".into(),
                ..indexed((1, 1), (1, 6), "a < b", "a <= b", Some(("…<…", "…<=…")))
            },
            None,
        );
        assert_eq!(boundary.changed, None);
        let json = serde_json::to_value(&boundary).unwrap();
        assert!(json.get("changed").is_none(), "{json}");
    }

    /// An equivalent mutant names its rule; a hint is listed beside the
    /// status it does not change.
    #[test]
    fn an_equivalent_mutant_names_its_rule_and_a_hint_its_kind() {
        let equivalent = MutantEntry::of(
            &IndexedMutant {
                status: MutantStatus::Equivalent,
                code: "zero-guarded".into(),
                ..indexed((1, 1), (1, 6), "n > 0", "n >= 0", None)
            },
            None,
        );
        let json = serde_json::to_value(&equivalent).unwrap();
        assert_eq!(json["status"], "equivalent");
        assert_eq!(
            json["weak"],
            serde_json::json!({
                "reach": "unknown",
                "infection": null,
                "code": "zero-guarded",
                "reason": "r",
                "rule": "zero-guarded",
            })
        );
        let hinted = MutantEntry::of(
            &IndexedMutant {
                untestable: Some((Untestable::Random, "reads Math.random()".into())),
                ..indexed((1, 1), (1, 6), "x < y", "x <= y", None)
            },
            None,
        );
        let json = serde_json::to_value(&hinted).unwrap();
        assert_eq!(json["status"], "not-infected");
        assert_eq!(json["weak"]["untestable"], "random");
        assert_eq!(json["weak"]["untestableReason"], "reads Math.random()");
        // StrykerJS killing an equivalent mutant is a rule's violation.
        assert!(unkilled(MutantStatus::Equivalent));
    }

    /// A mutant is on a change when a line it spans was added or modified,
    /// its first and last line included; a file git does not track yet is
    /// all added.
    #[test]
    fn a_mutant_is_on_a_change_where_its_span_meets_a_changed_line() {
        let diff = MutantDiff {
            changed: BTreeMap::from([(
                "src/a.ts".to_owned(),
                std::collections::BTreeSet::from([5, 9]),
            )]),
            untracked: std::collections::BTreeSet::from(["src/new.ts".to_owned()]),
            ..MutantDiff::default()
        };
        for (start, end, touches) in [
            (5, 5, true),
            (3, 5, true),
            (5, 8, true),
            (6, 8, false),
            (4, 4, false),
            (10, 12, false),
            (1, 20, true),
        ] {
            assert_eq!(
                diff.touches("src/a.ts", start, end),
                touches,
                "{start}-{end}"
            );
        }
        assert!(!diff.touches("src/b.ts", 5, 5));
        assert!(diff.touches("src/new.ts", 1, 1));
    }

    #[test]
    fn scores_leave_unknown_and_equivalent_mutants_out_and_are_null_over_nothing() {
        let summary = MutantsSummary::of(
            MutantCounts {
                not_reached: 1,
                not_infected: 1,
                infected: 2,
                unknown: 5,
                equivalent: 4,
            },
            1,
        );
        assert_eq!(summary.mutants, 13);
        assert_eq!(summary.weak_score, Some(0.5));
        assert_eq!(summary.weak_score_reached, Some(0.6667));
        assert_eq!(summary.mutation_score, None);
        assert_eq!(summary.untestable, 1);
        let none = MutantsSummary::of(
            MutantCounts {
                unknown: 3,
                equivalent: 2,
                ..MutantCounts::default()
            },
            0,
        );
        assert_eq!((none.weak_score, none.weak_score_reached), (None, None));
    }

    #[test]
    fn statuses_are_read_by_name_and_an_unknown_one_is_refused() {
        let options = serde_json::from_value::<MutantsOptions>(serde_json::json!({
            "statuses": ["not-infected", "not-reached"],
        }))
        .unwrap();
        assert_eq!(
            options.statuses,
            [MutantStatus::NotInfected, MutantStatus::NotReached]
        );
        let unknown = serde_json::from_value::<MutantsOptions>(serde_json::json!({
            "statuses": ["survived"],
        }))
        .unwrap_err();
        assert!(
            unknown
                .to_string()
                .contains("unknown mutant status survived"),
            "{unknown}"
        );
        // A status is a name, not a number.
        assert!(
            serde_json::from_value::<MutantsOptions>(serde_json::json!({ "statuses": [1] }))
                .is_err()
        );
    }

    /// A report is refused with the part it lacks named; one of no files,
    /// or of a file with no mutants, holds none.
    #[test]
    fn a_stryker_report_missing_a_part_is_refused_with_what_it_lacks() {
        let parse = |report: serde_json::Value| {
            parse_stryker_report(&report.to_string(), |path| format!("web/{path}"))
        };
        let location = serde_json::json!({
            "start": { "line": 1, "column": 1 },
            "end": { "line": 1, "column": 6 },
        });
        let mutant = |without: &str| {
            let mut mutant = serde_json::json!({
                "location": location,
                "mutatorName": "EqualityOperator",
                "replacement": "a <= b",
                "status": "Killed",
            });
            match without {
                "start" | "end" => {
                    mutant["location"].as_object_mut().unwrap().remove(without);
                }
                _ => {
                    mutant.as_object_mut().unwrap().remove(without);
                }
            }
            serde_json::json!({ "files": { "src/a.js": { "mutants": [mutant] } } })
        };
        assert!(parse_stryker_report("not json", str::to_owned).is_err());
        assert_eq!(
            parse(serde_json::json!({ "schemaVersion": "2" })).unwrap_err(),
            "it has no `files` object; is it StrykerJS's mutation.json?"
        );
        assert_eq!(
            parse(serde_json::json!({ "files": { "src/a.js": { "source": "" } } })).unwrap_err(),
            "src/a.js has no `mutants` list"
        );
        assert_eq!(
            parse(mutant("start")).unwrap_err(),
            "a mutant of src/a.js has no start location"
        );
        assert_eq!(
            parse(mutant("end")).unwrap_err(),
            "a mutant of src/a.js has no end location"
        );
        assert_eq!(
            parse(mutant("mutatorName")).unwrap_err(),
            "a mutant of src/a.js has no mutatorName"
        );
        assert_eq!(
            parse(mutant("status")).unwrap_err(),
            "a mutant of src/a.js has no status"
        );
        assert_eq!(parse(serde_json::json!({ "files": {} })).unwrap(), []);
        assert_eq!(
            parse(serde_json::json!({ "files": { "src/a.js": { "mutants": [] } } })).unwrap(),
            []
        );
        // Without the file's source, the mutated code is not known.
        assert_eq!(
            parse(mutant("none")).unwrap(),
            [StrykerReportMutant {
                file: "web/src/a.js".into(),
                start: (1, 1),
                end: (1, 6),
                mutator: "EqualityOperator".into(),
                replacement: Some("a <= b".into()),
                original: None,
                status: "Killed".into(),
                id: None,
            }]
        );
    }

    /// A lone carriage return ends a line; the column just past a source's
    /// last character is its end; a place outside the source has no text.
    #[test]
    fn a_slice_reaches_a_source_s_end_and_not_past_it() {
        assert_eq!(utf16_slice("a\rbc", (2, 1), (2, 3)).as_deref(), Some("bc"));
        assert_eq!(utf16_slice("ab\n", (2, 1), (2, 1)).as_deref(), Some(""));
        assert_eq!(utf16_slice("", (1, 1), (1, 1)).as_deref(), Some(""));
        assert_eq!(utf16_slice("ab", (0, 1), (1, 2)), None);
        assert_eq!(utf16_slice("ab", (1, 1), (3, 1)), None);
        assert_eq!(utf16_slice("ab", (1, 5), (1, 6)), None);
    }

    /// A report without the mutated code takes it from the run's mutant at
    /// the same place, or names the mutant by its replacement alone.
    #[test]
    fn a_stryker_mutant_without_its_code_takes_the_run_s_or_its_replacement() {
        assert!(stryker_by_id(&[], &HashMap::new()).is_empty());
        let mutant = |line: u32| StrykerReportMutant {
            file: "src/a.js".into(),
            start: (line, 5),
            end: (line, 10),
            mutator: "EqualityOperator".into(),
            replacement: Some("a <= b".into()),
            original: None,
            status: "Survived".into(),
            id: None,
        };
        let originals =
            HashMap::from([(("src/a.js", (1, 5), (1, 10), "EqualityOperator"), "a < b")]);
        // A report matched by what each mutant makes names Supercov's id:
        // the higher rank of its reports is kept.
        let by_id = |status: &str| StrykerReportMutant {
            id: Some("lib/a.rb:1:1-1:2:Send:nil".into()),
            status: status.into(),
            ..mutant(1)
        };
        let ranked = stryker_by_id(
            &[by_id("Survived"), by_id("Killed"), by_id("Survived")],
            &originals,
        );
        assert_eq!(ranked["lib/a.rb:1:1-1:2:Send:nil"].1, "Killed");
        let merged = stryker_by_id(&[mutant(1), mutant(2)], &originals);
        assert_eq!(
            merged.keys().collect::<Vec<_>>(),
            [
                "src/a.js:1:5-1:10:EqualityOperator:<=",
                "src/a.js:2:5-2:10:EqualityOperator:?a <= b",
            ]
        );
    }

    fn stryker(line: u32, mutator: &str, status: &str) -> StrykerReportMutant {
        StrykerReportMutant {
            file: "app/route.ts".into(),
            start: (line, 1),
            end: (line, 6),
            mutator: mutator.into(),
            replacement: Some("a <= b".into()),
            original: Some("a < b".into()),
            status: status.into(),
            id: None,
        }
    }

    fn compared(line: u32, status: MutantStatus) -> IndexedMutant {
        IndexedMutant {
            mutator: "EqualityOperator".into(),
            descriptor: "<=".into(),
            status,
            ..indexed((line, 1), (line, 6), "a < b", "a <= b", None)
        }
    }

    /// Past fifty, violations are counted and not listed; a survivor this
    /// run infects is not explained.
    #[test]
    fn the_join_lists_fifty_violations_and_counts_every_one() {
        let mut asked = (1..=51)
            .map(|line| compared(line, MutantStatus::NotInfected))
            .collect::<Vec<_>>();
        asked.push(compared(60, MutantStatus::Infected));
        let mut mutants = (1..=51)
            .map(|line| stryker(line, "EqualityOperator", "Killed"))
            .collect::<Vec<_>>();
        mutants.push(stryker(60, "EqualityOperator", "Survived"));
        let reports = StrykerReports {
            reports: vec!["reports/mutation.json".into()],
            mutants,
            ..StrykerReports::default()
        };
        let (joined, statuses) = join(&asked, &reports, |_| true);
        assert_eq!(joined.matched, 52);
        assert_eq!(joined.soundness_violations.total, 51);
        assert_eq!(joined.soundness_violations.mutants.len(), LISTED_VIOLATIONS);
        assert_eq!(
            joined.survivors_explained,
            SurvivorsExplained {
                survivors: 1,
                explained: 0
            }
        );
        assert_eq!(statuses.len(), 52);

        // Nothing asked: every StrykerJS mutant is its own.
        let (joined, _) = join(&[], &reports, |_| true);
        assert_eq!(joined.matched, 0);
        assert_eq!(joined.only_stryker.total, 52);
        // Nothing on either side.
        let (joined, statuses) = join(&[], &StrykerReports::default(), |_| true);
        assert_eq!(joined, StrykerJoin::default());
        assert!(statuses.is_empty());
    }

    /// Where no point contains a mutant whose status is unknown, the run
    /// cannot say whether it was reached.
    #[test]
    fn an_unplaced_mutant_is_of_unknown_reach() {
        let unplaced = MutantEntry::of(
            &IndexedMutant {
                status: MutantStatus::Unknown,
                code: "no-point".into(),
                ..indexed((1, 1), (1, 6), "a < b", "a <= b", None)
            },
            None,
        );
        assert_eq!(unplaced.weak.reach, "unknown");
        assert_eq!(unplaced.weak.infection, Some("unknown"));
        let placed = MutantEntry::of(
            &IndexedMutant {
                status: MutantStatus::Unknown,
                ..indexed((1, 1), (1, 6), "a < b", "a <= b", None)
            },
            None,
        );
        assert_eq!(placed.weak.reach, "reached");
    }

    fn report(mutation: Option<Vec<IndexedMutant>>) -> crate::coverage_report::CoverageReport {
        use crate::coverage_report::{
            CoverageManifest, CoverageReportRequest, ExitCodeInput, PointMeta, RawTestResult,
            RuntimeSnapshot, TestProvenance, analyze_coverage_results,
        };
        let mut report = analyze_coverage_results(&CoverageReportRequest {
            run_id: "run".into(),
            manifest: CoverageManifest {
                unmeasured: Vec::new(),
                decisions: Vec::new(),
                points: vec![PointMeta {
                    id: "point".into(),
                    kind: crate::coverage_analysis::PointKind::Statement,
                    file: "app/route.ts".into(),
                    line: 1,
                    column: 1,
                    source: "work();".into(),
                    label: None,
                }],
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
                assertion_sites: Vec::new(),
                probe_tables: Default::default(),
            },
            raw_results: vec![RawTestResult {
                duration_ms: None,
                span: None,
                test_id: Some("test".into()),
                scope: None,
                test: "test".into(),
                test_file: Some("tests/route.test.ts".into()),
                title: None,
                retry: Some(0),
                status: Some("passed".into()),
                expected_status: None,
                flaky: false,
                provenance: TestProvenance {
                    runner: "node:test".into(),
                    kind: "unit".into(),
                    project: None,
                    source: "runner-default".into(),
                    tags: Vec::new(),
                },
                role: "test".into(),
                attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
                phases: Vec::new(),
                runtime: vec![RuntimeSnapshot {
                    hits: vec!["point".into()],
                    ..Default::default()
                }],
                browser: Vec::new(),
                server: Vec::new(),
            }],
            generated_at: "time".into(),
            coverage_model: None,
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(0)),
        })
        .unwrap();
        // The scope includes a file with no mutants and leaves one out.
        let scope = serde_json::json!({
            "version": 1,
            "mode": "automatic",
            "roots": ["."],
            "entries": [
                { "file": "app/route.ts", "status": "included", "reason": "source" },
                { "file": "app/plain.ts", "status": "included", "reason": "source" },
                { "file": "app/skipped.ts", "status": "excluded", "reason": "generated" },
            ]
        });
        for view in [
            &mut report.view,
            &mut report.filters.passed,
            &mut report.filters.failed,
        ] {
            view.scope = Some(scope.clone());
        }
        report.mutation = mutation.map(|mutants| crate::coverage_index::MutationResults {
            mutant_set: "stryker-js@10.1.1".into(),
            mutants,
        });
        report
    }

    /// The index of `sections`, given to `body`.
    fn with_sections<R>(
        sections: &[crate::query_index::QueryIndexSection],
        body: impl FnOnce(&CoverageIndex<'_>) -> R,
    ) -> R {
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-mutants-query-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        ));
        std::fs::create_dir_all(&root).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        let identity = crate::query_index::QueryIndexIdentity {
            evidence_sha256: [1; 32],
            evidence_bytes: 100,
            analysis_sha256: [2; 32],
            producer_sha256: [3; 32],
            archive_schema_version: 2,
        };
        let path = root.join("query-index.v1.bin");
        crate::query_index::write_query_index(sections, &identity, &path).unwrap();
        let container = crate::query_index::QueryIndex::open(&path, &identity).unwrap();
        let index = CoverageIndex::new(&container).unwrap();
        body(&index)
    }

    /// The index of a run that listed `mutation`, given to `body`.
    fn with_index<R>(
        mutation: Option<Vec<IndexedMutant>>,
        body: impl FnOnce(&CoverageIndex<'_>) -> R,
    ) -> R {
        let sections = crate::coverage_index::coverage_index_sections(&report(mutation)).unwrap();
        with_sections(&sections, body)
    }

    /// Two files' mutants: one not infected at a clock boundary, one not
    /// reached and one infected in `app/route.ts`, and one of unknown status
    /// in `app/router.ts`.
    fn listed() -> Vec<IndexedMutant> {
        vec![
            IndexedMutant {
                untestable: Some((Untestable::Clock, "reads Date.now()".into())),
                ..compared(1, MutantStatus::NotInfected)
            },
            IndexedMutant {
                start: (10, 3),
                end: (10, 20),
                status: MutantStatus::NotReached,
                ..indexed((10, 3), (10, 20), "items.filter(ok)", "items", None)
            },
            compared(20, MutantStatus::Infected),
            IndexedMutant {
                file: "app/router.ts".into(),
                status: MutantStatus::Unknown,
                code: "no-point".into(),
                ..compared(3, MutantStatus::Unknown)
            },
        ]
    }

    fn query(
        index: &CoverageIndex<'_>,
        file: Option<&str>,
        options: &MutantsOptions,
    ) -> Result<(CoverageMutantsData, AgentPagination), QueryError> {
        coverage_mutants_query(
            index,
            CoverageMutantsQueryOptions {
                run: "run",
                file,
                line: None,
                options,
                offset: 0,
                limit: 50,
            },
        )
    }

    #[test]
    fn a_page_of_no_mutants_is_refused() {
        with_index(Some(listed()), |index| {
            let refused = coverage_mutants_query(
                index,
                CoverageMutantsQueryOptions {
                    run: "run",
                    file: None,
                    line: None,
                    options: &MutantsOptions::default(),
                    offset: 0,
                    limit: 0,
                },
            );
            assert!(matches!(refused, Err(QueryError::InvalidPagination)));
        });
    }

    /// Each read of the index the query makes fails it when the index
    /// cannot be read.
    #[test]
    fn a_mutants_query_fails_where_the_index_cannot_be_read() {
        with_index(Some(listed()), |index| {
            let options = MutantsOptions::default();
            for (fault, file) in [
                ("coverage-index-read.mutation", None),
                ("coverage-index-read.mutants_of", None),
                ("coverage-index-read.scope_entries", Some("app/plain.ts")),
            ] {
                let failed = crate::fault::during(fault, || query(index, file, &options));
                assert!(matches!(failed, Err(QueryError::Index(_))), "{fault}");
            }
        });
    }

    /// A file is named whole or by a part only its name has; a part two
    /// names share is ambiguous.
    #[test]
    fn a_file_is_found_by_a_part_of_its_name_only_it_has() {
        with_index(Some(listed()), |index| {
            let options = MutantsOptions::default();
            let (data, _) = query(index, Some("router"), &options).unwrap();
            assert_eq!(data.file.as_deref(), Some("app/router.ts"));
            assert_eq!(data.mutants.len(), 1);
            assert_eq!(data.mutants[0].weak.reach, "unknown");
            let (data, _) = query(index, Some("app/route.ts"), &options).unwrap();
            assert_eq!(data.file.as_deref(), Some("app/route.ts"));
            assert_eq!(data.mutants.len(), 3);
            // A clock boundary's not-infected mutant is counted untestable.
            assert_eq!(data.summary.untestable, 1);
            let Err(QueryError::AmbiguousSelector { selector, matches }) =
                query(index, Some("app/rout"), &options)
            else {
                panic!("a part two files share is ambiguous");
            };
            assert_eq!(selector, "app/rout");
            assert_eq!(matches, ["app/route.ts", "app/router.ts"]);
        });
    }

    /// A file the run's scope includes and no mutant is in has a note; one
    /// it leaves out, or does not name, is not found, as is any file of a
    /// run that listed no mutants.
    #[test]
    fn a_file_the_scope_includes_with_no_mutants_has_a_note() {
        with_index(Some(listed()), |index| {
            let options = MutantsOptions::default();
            let (data, _) = query(index, Some("app/plain.ts"), &options).unwrap();
            assert_eq!(
                data.note.as_deref(),
                Some("app/plain.ts has no mutants in this run.")
            );
            assert_eq!(data.file.as_deref(), Some("app/plain.ts"));
            assert!(data.mutants.is_empty());
            for selector in ["app/skipped.ts", "app/nowhere.ts"] {
                assert!(
                    matches!(
                        query(index, Some(selector), &options),
                        Err(QueryError::SourceNotFound(file)) if file == selector
                    ),
                    "{selector}"
                );
            }
        });
        with_index(None, |index| {
            assert!(matches!(
                query(index, Some("app/plain.ts"), &MutantsOptions::default()),
                Err(QueryError::SourceNotFound(_))
            ));
            let (data, _) = query(index, None, &MutantsOptions::default()).unwrap();
            assert_eq!(data.note.as_deref(), Some(NO_LIST));
        });
    }

    #[test]
    fn a_run_that_listed_mutants_in_no_file_says_so() {
        with_index(Some(Vec::new()), |index| {
            let (data, _) = query(index, None, &MutantsOptions::default()).unwrap();
            assert_eq!(
                data.note.as_deref(),
                Some("The run listed its mutants, and the files it measured have none.")
            );
            assert_eq!(data.mutant_set.as_deref(), Some("stryker-js@10.1.1"));
            assert!(data.mutants.is_empty());
        });
    }

    /// A mutator asked for leaves out the file's other mutants, and the
    /// StrykerJS mutants of other mutators and other files.
    #[test]
    fn a_mutator_asked_for_leaves_out_the_other_mutators_mutants() {
        with_index(Some(listed()), |index| {
            let options = MutantsOptions {
                mutator: Some("equalityoperator".into()),
                stryker: Some(StrykerReports {
                    mutants: vec![
                        stryker(1, "EqualityOperator", "Killed"),
                        stryker(10, "MethodExpression", "Survived"),
                        StrykerReportMutant {
                            file: "app/router.ts".into(),
                            ..stryker(3, "EqualityOperator", "Killed")
                        },
                    ],
                    ..StrykerReports::default()
                }),
                ..MutantsOptions::default()
            };
            let (data, _) = query(index, Some("app/route.ts"), &options).unwrap();
            assert_eq!(
                data.mutants
                    .iter()
                    .map(|mutant| mutant.span.start[0])
                    .collect::<Vec<_>>(),
                [1, 20]
            );
            let joined = data.stryker.unwrap();
            assert_eq!(joined.matched, 1);
            assert_eq!(joined.only_stryker.total, 0);
            assert_eq!(joined.only_supercov.total, 1);
            assert_eq!(joined.soundness_violations.total, 1);
            assert_eq!(data.mutants[0].stryker.as_deref(), Some("Killed"));
        });
    }

    /// With `--diff`, the mutants on changed lines are listed what the
    /// change's tests leave alive first, and the summary counts the changed
    /// lines a mutant is on: a file git does not track yet by its mutants'
    /// lines.
    #[test]
    fn a_change_s_mutants_are_counted_by_the_lines_they_are_on() {
        with_index(Some(listed()), |index| {
            let options = MutantsOptions {
                diff: Some(MutantDiff {
                    base: "origin/main".into(),
                    merge_base: "abc".into(),
                    commit: None,
                    changed: BTreeMap::from([(
                        "app/route.ts".to_owned(),
                        std::collections::BTreeSet::from([1, 2, 10]),
                    )]),
                    untracked: std::collections::BTreeSet::from([
                        "app/router.ts".to_owned(),
                        "app/new.ts".to_owned(),
                    ]),
                }),
                // StrykerJS's mutant on line 20 is off the change.
                stryker: Some(StrykerReports {
                    mutants: vec![
                        stryker(1, "EqualityOperator", "Survived"),
                        stryker(20, "EqualityOperator", "Killed"),
                    ],
                    ..StrykerReports::default()
                }),
                ..MutantsOptions::default()
            };
            let (data, pagination) = query(index, None, &options).unwrap();
            assert_eq!(
                data.mutants
                    .iter()
                    .map(|mutant| (mutant.file.as_str(), mutant.status))
                    .collect::<Vec<_>>(),
                [
                    ("app/route.ts", MutantStatus::NotInfected),
                    ("app/route.ts", MutantStatus::NotReached),
                    ("app/router.ts", MutantStatus::Unknown),
                ]
            );
            assert_eq!(pagination.total, 3);
            let diff = data.diff.unwrap();
            assert_eq!(diff.changed_lines, 4);
            assert_eq!(diff.lines_with_mutants, 3);
            let joined = data.stryker.unwrap();
            assert_eq!(joined.matched, 1);
            assert_eq!(joined.only_stryker.total, 0);
            assert_eq!(
                data.files
                    .iter()
                    .map(|row| row.file.as_str())
                    .collect::<Vec<_>>(),
                ["app/route.ts", "app/router.ts"]
            );
        });
    }

    /// A change no mutant is on lists none, and strong verdicts of none
    /// give no score.
    #[test]
    fn a_change_no_mutant_is_on_lists_none() {
        with_index(Some(listed()), |index| {
            let options = MutantsOptions {
                diff: Some(MutantDiff::default()),
                strong: Some(BTreeMap::new()),
                ..MutantsOptions::default()
            };
            let (data, _) = query(index, None, &options).unwrap();
            assert!(data.mutants.is_empty());
            assert!(data.files.is_empty());
            assert_eq!(data.summary.mutation_score, None);
            assert!(!data.levels.strong);
            let diff = data.diff.unwrap();
            assert_eq!((diff.changed_lines, diff.lines_with_mutants), (0, 0));
        });
    }

    /// An index that lists a file and none of its mutants is read as a file
    /// with none to ask about.
    #[test]
    fn a_file_listed_with_no_mutants_has_none_to_ask_about() {
        use crate::coverage_index::{SECTION_MUTANTS, SECTION_MUTATION_FILES};
        let mut sections =
            crate::coverage_index::coverage_index_sections(&report(Some(vec![compared(
                1,
                MutantStatus::NotInfected,
            )])))
            .unwrap();
        for section in &mut sections {
            if section.kind == SECTION_MUTATION_FILES {
                // Its counts and its mutants' count, from the not-infected
                // one at offset 12 to the total at 28.
                section.bytes[12..16].fill(0);
                section.bytes[28..32].fill(0);
            } else if section.kind == SECTION_MUTANTS {
                section.bytes.clear();
                section.count = 0;
            }
        }
        with_sections(&sections, |index| {
            let (data, _) = query(index, Some("app/route.ts"), &MutantsOptions::default()).unwrap();
            assert_eq!(data.file.as_deref(), Some("app/route.ts"));
            assert!(data.mutants.is_empty());
            assert_eq!(data.summary.mutants, 0);
        });
    }
}
