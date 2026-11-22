//! What every mutation report says of a run's mutants, whatever their
//! language: each mutant's strong outcome and, where it has none, why; the
//! counts and scores of some mutants; and the two formats other mutation
//! tools read, StrykerJS's `mutation.json` (the mutation testing report
//! schema) and PIT's `mutations.xml`.
//!
//! The HTML report, the markdown table and both formats read a mutant the
//! same way, from `mutants`' own entries, so they never disagree.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::coverage_index::{MutantCounts, MutantStatus};
use crate::mutants_query::{CoverageMutantsData, MutantEntry};
use crate::strong_plan::Verdict;

/// How `mutants test` says a mutant's language has no driver yet.
const NO_DRIVER: &str = "no strong mutation driver for ";

/// Where a mutant got to in a strong pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Outcome {
    Killed,
    Timeout,
    Survived,
    NoCoverage,
    Equivalent,
    /// The mutant does not compile.
    Unviable,
    /// The pass could not give it a verdict.
    Error,
    /// No driver runs its language's tests yet.
    NotRun,
    /// No strong verdict: no pass ran it.
    Pending,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Killed => "killed",
            Self::Timeout => "timeout",
            Self::Survived => "survived",
            Self::NoCoverage => "no-coverage",
            Self::Equivalent => "equivalent",
            Self::Unviable => "unviable",
            Self::Error => "error",
            Self::NotRun => "not-run",
            Self::Pending => "pending",
        }
    }
}

/// A mutant's saved strong verdict, where a pass gave one.
pub fn verdict(entry: &MutantEntry) -> Option<Verdict> {
    entry
        .strong
        .as_ref()
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

/// The language whose driver `mutants test` lacked, from its verdict.
fn undriven(verdict: &Verdict) -> Option<&str> {
    let reason = verdict.reason.as_deref()?.strip_prefix(NO_DRIVER)?;
    Some(reason.strip_suffix(" yet").unwrap_or(reason))
}

/// A mutant's strong outcome.
pub fn outcome(entry: &MutantEntry) -> Outcome {
    let Some(verdict) = verdict(entry) else {
        return Outcome::Pending;
    };
    match verdict.status.as_str() {
        "killed" => Outcome::Killed,
        "timeout" => Outcome::Timeout,
        "survived" => Outcome::Survived,
        "no-coverage" => Outcome::NoCoverage,
        "equivalent" => Outcome::Equivalent,
        "unviable" => Outcome::Unviable,
        _ if undriven(&verdict).is_some() => Outcome::NotRun,
        _ => Outcome::Error,
    }
}

/// A mutant the tests leave alive: one a strong pass saw survive or no test
/// reach, or, with no strong verdict, one weak mutation already shows no
/// test can kill (not infected or not reached), as the pass itself decides
/// such mutants without running them.
pub fn alive(entry: &MutantEntry) -> bool {
    match outcome(entry) {
        Outcome::Survived | Outcome::NoCoverage => true,
        Outcome::Pending => matches!(
            entry.status,
            MutantStatus::NotInfected | MutantStatus::NotReached
        ),
        _ => false,
    }
}

fn tests(count: u64) -> String {
    format!("{count} test{}", if count == 1 { "" } else { "s" })
}

/// Why a mutant has its outcome, in plain words. `strong_ran` is whether a
/// strong pass gave any of the run's mutants a verdict: a mutant without
/// one is then one the pass left out.
pub fn why(entry: &MutantEntry, strong_ran: bool) -> String {
    let saved = verdict(entry);
    let given = saved.as_ref().and_then(|verdict| verdict.reason.clone());
    let ran = saved.as_ref().map_or(0, |verdict| verdict.tests);
    match outcome(entry) {
        Outcome::Killed => match saved.and_then(|verdict| verdict.killed_by) {
            Some(test) => format!("{test} failed with it in place"),
            None => "a test failed with it in place".into(),
        },
        Outcome::Timeout => {
            given.unwrap_or_else(|| "a test ran past its time with it in place".into())
        }
        Outcome::Survived => given.unwrap_or_else(|| {
            format!(
                "every test that reached it passed with it in place ({} ran)",
                tests(ran)
            )
        }),
        Outcome::NoCoverage => given.unwrap_or_else(|| "no test reaches it".into()),
        Outcome::Equivalent => given.unwrap_or_else(|| entry.weak.reason.clone()),
        Outcome::Unviable => given.unwrap_or_else(|| "the mutant does not compile".into()),
        Outcome::Error => given.unwrap_or_else(|| "the pass could not give it a verdict".into()),
        Outcome::NotRun => format!(
            "not run: no driver for {} yet",
            saved.as_ref().and_then(undriven).unwrap_or_default()
        ),
        Outcome::Pending => match entry.status {
            MutantStatus::Infected | MutantStatus::Unknown if strong_ran => format!(
                "no strong verdict: the strong pass did not run it ({}); `mutants test` runs it",
                entry.weak.reason
            ),
            MutantStatus::Infected | MutantStatus::Unknown => format!(
                "no strong verdict yet ({}); `mutants test` runs it",
                entry.weak.reason
            ),
            _ => entry.weak.reason.clone(),
        },
    }
}

/// Mutants by strong outcome.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrongCounts {
    pub killed: usize,
    pub timeout: usize,
    pub survived: usize,
    pub no_coverage: usize,
    pub equivalent: usize,
    pub unviable: usize,
    pub error: usize,
    pub not_run: usize,
    pub pending: usize,
}

impl StrongCounts {
    pub fn add(&mut self, outcome: Outcome) {
        *match outcome {
            Outcome::Killed => &mut self.killed,
            Outcome::Timeout => &mut self.timeout,
            Outcome::Survived => &mut self.survived,
            Outcome::NoCoverage => &mut self.no_coverage,
            Outcome::Equivalent => &mut self.equivalent,
            Outcome::Unviable => &mut self.unviable,
            Outcome::Error => &mut self.error,
            Outcome::NotRun => &mut self.not_run,
            Outcome::Pending => &mut self.pending,
        } += 1;
    }

    /// The mutants a pass gave a verdict, a driverless one's included.
    pub fn verdicts(&self) -> usize {
        self.killed
            + self.timeout
            + self.survived
            + self.no_coverage
            + self.equivalent
            + self.unviable
            + self.error
            + self.not_run
    }

    /// StrykerJS's mutation score: killed and timed out over those and the
    /// survivors and uncovered; `None` before any of them.
    pub fn score(&self) -> Option<f64> {
        let detected = self.killed + self.timeout;
        let total = detected + self.survived + self.no_coverage;
        (total > 0).then(|| (detected as f64 / total as f64 * 10_000.0).round() / 10_000.0)
    }
}

/// Some mutants' counts and scores, weak and strong.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Tally {
    pub mutants: usize,
    pub weak: MutantCounts,
    pub strong: StrongCounts,
    /// What the tests leave alive (see [`alive`]).
    pub survivors: usize,
    pub weak_score: Option<f64>,
    pub mutation_score: Option<f64>,
}

impl Tally {
    pub fn of<'a>(entries: impl IntoIterator<Item = &'a MutantEntry>) -> Self {
        let mut tally = Self::default();
        for entry in entries {
            tally.mutants += 1;
            tally.weak.add(entry.status);
            tally.strong.add(outcome(entry));
            tally.survivors += usize::from(alive(entry));
        }
        let weak = &tally.weak;
        let denominator = weak.infected + weak.not_infected + weak.not_reached;
        tally.weak_score = (denominator > 0)
            .then(|| (weak.infected as f64 / denominator as f64 * 10_000.0).round() / 10_000.0);
        tally.mutation_score = tally.strong.score();
        tally
    }
}

/// The language a mutant's file is in, by its extension, as the mutation
/// testing report schema names languages.
pub fn language(file: &str) -> &'static str {
    let extension = file.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension.to_ascii_lowercase().as_str() {
        "ts" | "tsx" | "mts" | "cts" => "typescript",
        "py" | "pyi" => "python",
        "rs" => "rust",
        "go" => "go",
        "rb" => "ruby",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "js" | "jsx" | "mjs" | "cjs" => "javascript",
        _ => "other",
    }
}

/// Each group's tally, by the key `group` gives a mutant.
pub fn tallies<'a>(
    entries: &'a [MutantEntry],
    group: impl Fn(&'a MutantEntry) -> &'a str,
) -> BTreeMap<&'a str, Tally> {
    let mut grouped = BTreeMap::<&str, Vec<&MutantEntry>>::new();
    for entry in entries {
        grouped.entry(group(entry)).or_default().push(entry);
    }
    grouped
        .into_iter()
        .map(|(key, entries)| (key, Tally::of(entries)))
        .collect()
}

/// What an HTML report shows of some mutants, whatever their language: the
/// weak summary, a tally of them all, of each language and of each file, the
/// change they were narrowed to, and every mutant with both levels and why.
/// The interactive report and the static one read the same section.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub mutant_set: String,
    pub summary: crate::mutants_query::MutantsSummary,
    pub tally: Tally,
    /// Whether a strong pass gave any of them a verdict.
    pub strong: bool,
    pub languages: BTreeMap<String, Tally>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<crate::mutants_query::DiffSummary>,
    pub files: BTreeMap<String, Tally>,
    pub mutants: Vec<SectionMutant>,
}

/// One mutant as a report shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionMutant {
    pub id: String,
    pub file: String,
    pub line: u32,
    pub column: u32,
    pub end_line: u32,
    pub mutator: String,
    /// `original → replacement`, or the part it changes.
    pub change: String,
    pub status: MutantStatus,
    pub reason: String,
    pub code: String,
    /// `clock` or `random`: practically out of a test's reach.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub untestable: Option<crate::coverage_index::Untestable>,
    pub outcome: Outcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub killed_by: Option<String>,
    /// How many tests the strong pass ran against it, and for how long.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// Why it has its outcome, in plain words.
    pub why: String,
    /// Left alive by the tests.
    pub alive: bool,
}

/// The section of every mutant `data` holds; `None` for a run that listed no
/// mutants, or none where it was not narrowed to a change (a change with
/// none on its lines still says so).
pub fn section(data: CoverageMutantsData) -> Option<Section> {
    let mutant_set = data.mutant_set.clone()?;
    if data.summary.mutants == 0 && data.diff.is_none() {
        return None;
    }
    let strong = data.mutants.iter().any(|mutant| mutant.strong.is_some());
    let owned = |tallies: BTreeMap<&str, Tally>| {
        tallies
            .into_iter()
            .map(|(key, tally)| (key.to_owned(), tally))
            .collect::<BTreeMap<_, _>>()
    };
    let mutants = data
        .mutants
        .iter()
        .map(|mutant| {
            let saved = verdict(mutant);
            SectionMutant {
                id: mutant.id.clone(),
                file: mutant.file.clone(),
                line: mutant.span.start[0],
                column: mutant.span.start[1],
                end_line: mutant.span.end[0],
                mutator: mutant.mutator.clone(),
                change: change(mutant),
                status: mutant.status,
                reason: mutant.weak.reason.clone(),
                code: mutant.weak.code.clone(),
                untestable: mutant.weak.untestable,
                outcome: outcome(mutant),
                killed_by: saved.as_ref().and_then(|verdict| verdict.killed_by.clone()),
                tests: saved.as_ref().map(|verdict| verdict.tests),
                duration_ms: saved.as_ref().map(|verdict| verdict.duration_ms),
                why: why(mutant, strong),
                alive: alive(mutant),
            }
        })
        .collect();
    Some(Section {
        mutant_set,
        tally: Tally::of(&data.mutants),
        strong,
        languages: owned(tallies(&data.mutants, |mutant| language(&mutant.file))),
        files: owned(tallies(&data.mutants, |mutant| mutant.file.as_str())),
        summary: data.summary,
        diff: data.diff,
        mutants,
    })
}

/// The thresholds StrykerJS reports by default.
const THRESHOLDS: (u8, u8) = (80, 60);

/// A mutant's status in the mutation testing report schema.
fn stryker_status(entry: &MutantEntry) -> &'static str {
    match outcome(entry) {
        Outcome::Killed => "Killed",
        Outcome::Timeout => "Timeout",
        Outcome::Survived => "Survived",
        Outcome::NoCoverage => "NoCoverage",
        Outcome::Equivalent => "Ignored",
        Outcome::Unviable => "CompileError",
        Outcome::Error => "RuntimeError",
        Outcome::NotRun => "Pending",
        Outcome::Pending => match entry.status {
            MutantStatus::NotReached => "NoCoverage",
            MutantStatus::NotInfected => "Survived",
            MutantStatus::Equivalent => "Ignored",
            MutantStatus::Infected | MutantStatus::Unknown => "Pending",
        },
    }
}

/// The mutants as StrykerJS's `mutation.json`, the mutation testing report
/// schema's version 2, which mutation-testing-elements, the Stryker
/// Dashboard and CI integrations read. A file is written with its source,
/// which the schema requires; a file `sources` lacks is left out and named
/// in the second value.
pub fn stryker_report(
    data: &CoverageMutantsData,
    sources: &BTreeMap<String, String>,
) -> (Value, Vec<String>) {
    let strong_ran = data.mutants.iter().any(|entry| entry.strong.is_some());
    let mut files = serde_json::Map::new();
    let mut missing = Vec::new();
    for (file, entries) in &by_file(&data.mutants) {
        let Some(source) = sources.get(*file) else {
            missing.push((*file).to_owned());
            continue;
        };
        let mutants = entries
            .iter()
            .map(|entry| {
                let mut mutant = json!({
                    "id": entry.id,
                    "mutatorName": entry.mutator,
                    "replacement": entry.replacement,
                    "description": change(entry),
                    "location": {
                        "start": { "line": entry.span.start[0].max(1), "column": entry.span.start[1].max(1) },
                        "end": { "line": entry.span.end[0].max(1), "column": entry.span.end[1].max(1) },
                    },
                    "status": stryker_status(entry),
                    "statusReason": why(entry, strong_ran),
                });
                if let Some(verdict) = verdict(entry) {
                    mutant["testsCompleted"] = json!(verdict.tests);
                    mutant["duration"] = json!(verdict.duration_ms);
                    if let Some(test) = verdict.killed_by {
                        mutant["killedBy"] = json!([test]);
                    }
                }
                mutant
            })
            .collect::<Vec<_>>();
        files.insert(
            (*file).to_owned(),
            json!({ "language": language(file), "source": source, "mutants": mutants }),
        );
    }
    let report = json!({
        "schemaVersion": "2",
        "thresholds": { "high": THRESHOLDS.0, "low": THRESHOLDS.1 },
        "files": files,
        "framework": {
            "name": "Supercov",
            "version": env!("CARGO_PKG_VERSION"),
            "dependencies": { "mutantSet": data.mutant_set.clone().unwrap_or_default() },
        },
    });
    (report, missing)
}

/// `original → replacement`, or the part a mutant changes.
pub fn change(entry: &MutantEntry) -> String {
    match &entry.changed {
        Some(changed) => format!("{} → {}", changed.from, changed.to),
        None => format!("{} → {}", entry.original, entry.replacement),
    }
}

fn by_file(entries: &[MutantEntry]) -> BTreeMap<&str, Vec<&MutantEntry>> {
    let mut files = BTreeMap::<&str, Vec<&MutantEntry>>::new();
    for entry in entries {
        files.entry(entry.file.as_str()).or_default().push(entry);
    }
    files
}

/// A mutant's status in PIT's `mutations.xml`, and whether PIT counts it
/// detected; `None` for an equivalent mutant, which PIT has no status for
/// and which no score counts.
fn pit_status(entry: &MutantEntry) -> Option<(&'static str, bool)> {
    Some(match outcome(entry) {
        Outcome::Killed => ("KILLED", true),
        Outcome::Timeout => ("TIMED_OUT", true),
        Outcome::Survived => ("SURVIVED", false),
        Outcome::NoCoverage => ("NO_COVERAGE", false),
        Outcome::Unviable => ("NON_VIABLE", false),
        // A pass that gave no verdict did not run the mutant to its end:
        // PIT's RUN_ERROR would count it detected.
        Outcome::Error | Outcome::NotRun => ("NOT_STARTED", false),
        Outcome::Equivalent => return None,
        Outcome::Pending => match entry.status {
            MutantStatus::NotReached => ("NO_COVERAGE", false),
            MutantStatus::NotInfected => ("SURVIVED", false),
            MutantStatus::Infected | MutantStatus::Unknown => ("NOT_STARTED", false),
            MutantStatus::Equivalent => return None,
        },
    })
}

/// Text for XML character data or a quoted attribute, without the control
/// characters XML 1.0 cannot hold.
fn xml_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(' '),
            c if c < ' ' => {}
            c => out.push(c),
        }
    }
    out
}

/// The class PIT would name a file's code by: the path after a `java` or
/// `kotlin` source root, or the whole path, without its extension, dotted.
fn class_of(file: &str) -> String {
    let stem = file.rsplit_once('.').map_or(file, |(stem, _)| stem);
    let after_root = ["/java/", "/kotlin/"]
        .iter()
        .filter_map(|root| stem.rfind(root).map(|at| &stem[at + root.len()..]))
        .min_by_key(|rest| rest.len());
    after_root.unwrap_or(stem).replace(['/', '\\'], ".")
}

/// The mutants as PIT's `mutations.xml`, which Sonar's PIT plugin and
/// PIT's own report aggregator read. Supercov mutates source, not bytecode:
/// a mutant's method and its descriptor are left empty, its class is its
/// file's (see [`class_of`]), and its index is its place in its file, which
/// with the class names it as PIT's index names an instruction of a method.
pub fn pit_xml(data: &CoverageMutantsData) -> String {
    let strong_ran = data.mutants.iter().any(|entry| entry.strong.is_some());
    let mut xml =
        String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mutations partial=\"false\">\n");
    let mut in_file = BTreeMap::<&str, usize>::new();
    for entry in &data.mutants {
        let Some((status, detected)) = pit_status(entry) else {
            continue;
        };
        let index = in_file.entry(entry.file.as_str()).or_default();
        let saved = verdict(entry);
        let source_file = entry.file.rsplit(['/', '\\']).next().unwrap_or_default();
        xml.push_str(&format!(
            "<mutation detected=\"{detected}\" status=\"{status}\" numberOfTestsRun=\"{}\"><sourceFile>{}</sourceFile><mutatedClass>{}</mutatedClass><mutatedMethod></mutatedMethod><methodDescription></methodDescription><lineNumber>{}</lineNumber><mutator>{}</mutator><indexes><index>{index}</index></indexes><blocks><block>0</block></blocks><killingTest>{}</killingTest><description>{}</description></mutation>\n",
            saved.as_ref().map_or(0, |verdict| verdict.tests),
            xml_text(source_file),
            xml_text(&class_of(&entry.file)),
            entry.span.start[0],
            xml_text(&entry.mutator),
            xml_text(saved.and_then(|verdict| verdict.killed_by).as_deref().unwrap_or_default()),
            xml_text(&format!("{}: {}", change(entry), why(entry, strong_ran))),
        ));
        *index += 1;
    }
    xml.push_str("</mutations>\n");
    xml
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutants_query::{
        MutantChange, MutantLevels, MutantSpan, MutantsSummary, WeakResult,
    };

    fn entry(file: &str, line: u32, status: MutantStatus, strong: Option<Value>) -> MutantEntry {
        MutantEntry {
            id: format!("{file}:{line}:5-{line}:9:EqualityOperator:<"),
            file: file.into(),
            span: MutantSpan {
                start: [line, 5],
                end: [line, 9],
            },
            mutator: "EqualityOperator".into(),
            original: "a <= b".into(),
            replacement: "a < b".into(),
            changed: None,
            status,
            weak: WeakResult {
                reach: "reached",
                infection: None,
                code: "code".into(),
                reason: format!("weak {}", status.name()),
                proxy: false,
                rule: None,
                untestable: None,
                untestable_reason: None,
            },
            strong,
            stryker: None,
        }
    }

    fn data(mutants: Vec<MutantEntry>) -> CoverageMutantsData {
        CoverageMutantsData {
            run: "run".into(),
            mutant_set: Some("set@1".into()),
            levels: MutantLevels {
                weak: true,
                strong: false,
            },
            file: None,
            line: None,
            summary: MutantsSummary::of(MutantCounts::default(), 0),
            note: None,
            mutants,
            stryker: None,
            files: Vec::new(),
            diff: None,
            fail_on: None,
        }
    }

    #[test]
    fn each_verdict_is_an_outcome_with_its_reason_in_plain_words() {
        use MutantStatus::*;
        let strong_ran = true;
        let cases = [
            (
                json!({"status": "killed", "killedBy": "t > a", "tests": 1}),
                Outcome::Killed,
                "t > a failed with it in place",
            ),
            (
                json!({"status": "killed", "tests": 1}),
                Outcome::Killed,
                "a test failed with it in place",
            ),
            (
                json!({"status": "timeout"}),
                Outcome::Timeout,
                "a test ran past its time with it in place",
            ),
            (
                json!({"status": "timeout", "reason": "looped"}),
                Outcome::Timeout,
                "looped",
            ),
            (
                json!({"status": "survived", "tests": 1}),
                Outcome::Survived,
                "every test that reached it passed with it in place (1 test ran)",
            ),
            (
                json!({"status": "survived", "tests": 3}),
                Outcome::Survived,
                "every test that reached it passed with it in place (3 tests ran)",
            ),
            (
                json!({"status": "no-coverage"}),
                Outcome::NoCoverage,
                "no test reaches it",
            ),
            (
                json!({"status": "equivalent"}),
                Outcome::Equivalent,
                "weak infected",
            ),
            (
                json!({"status": "unviable"}),
                Outcome::Unviable,
                "the mutant does not compile",
            ),
            (
                json!({"status": "error"}),
                Outcome::Error,
                "the pass could not give it a verdict",
            ),
            (
                json!({"status": "error", "reason": "something else"}),
                Outcome::Error,
                "something else",
            ),
            (
                json!({"status": "error", "reason": "no strong mutation driver for cobol yet"}),
                Outcome::NotRun,
                "not run: no driver for cobol yet",
            ),
            (
                json!({"status": "error", "reason": "no strong mutation driver for cobol"}),
                Outcome::NotRun,
                "not run: no driver for cobol yet",
            ),
            (
                json!({"status": "strange"}),
                Outcome::Error,
                "the pass could not give it a verdict",
            ),
        ];
        for (verdict, expected, reason) in cases {
            let mutant = entry("a.js", 1, Infected, Some(verdict.clone()));
            assert_eq!(outcome(&mutant), expected, "{verdict}");
            assert_eq!(why(&mutant, strong_ran), reason, "{verdict}");
        }
        // Not a verdict at all.
        assert_eq!(
            outcome(&entry("a.js", 1, Infected, Some(json!(3)))),
            Outcome::Pending
        );
        // No verdict: why from weak mutation, and whether a pass ran.
        assert_eq!(
            why(&entry("a.js", 1, NotReached, None), false),
            "weak not-reached"
        );
        assert_eq!(
            why(&entry("a.js", 1, Infected, None), false),
            "no strong verdict yet (weak infected); `mutants test` runs it"
        );
        assert_eq!(
            why(&entry("a.js", 1, Unknown, None), true),
            "no strong verdict: the strong pass did not run it (weak unknown); `mutants test` runs it"
        );
        let names = [
            Outcome::Killed,
            Outcome::Timeout,
            Outcome::Survived,
            Outcome::NoCoverage,
            Outcome::Equivalent,
            Outcome::Unviable,
            Outcome::Error,
            Outcome::NotRun,
            Outcome::Pending,
        ]
        .map(Outcome::name);
        assert_eq!(
            names.join(","),
            "killed,timeout,survived,no-coverage,equivalent,unviable,error,not-run,pending"
        );
    }

    #[test]
    fn survivors_are_what_no_test_can_kill_and_the_scores_count_them() {
        use MutantStatus::*;
        let mutants = vec![
            entry("a.js", 1, Infected, Some(json!({"status": "killed"}))),
            entry("a.js", 2, Infected, Some(json!({"status": "timeout"}))),
            entry("a.js", 3, Infected, Some(json!({"status": "survived"}))),
            entry(
                "b.py",
                1,
                NotReached,
                Some(json!({"status": "no-coverage"})),
            ),
            entry("b.py", 2, NotInfected, None),
            entry("b.py", 3, NotReached, None),
            entry("b.py", 4, Infected, None),
            entry("b.py", 5, Equivalent, Some(json!({"status": "equivalent"}))),
            entry("b.py", 6, Infected, Some(json!({"status": "unviable"}))),
            entry("b.py", 7, Infected, Some(json!({"status": "error"}))),
            entry(
                "b.py",
                8,
                Infected,
                Some(
                    json!({"status": "error", "reason": "no strong mutation driver for cobol yet"}),
                ),
            ),
        ];
        let alive = mutants.iter().filter(|mutant| alive(mutant)).count();
        assert_eq!(alive, 4);
        let tally = Tally::of(&mutants);
        assert_eq!(tally.mutants, 11);
        assert_eq!(tally.survivors, 4);
        assert_eq!(tally.strong.verdicts(), 8);
        assert_eq!(tally.strong.pending, 3);
        assert_eq!(tally.mutation_score, Some(0.5));
        // Infected 7 of 7 + 1 not infected + 2 not reached.
        assert_eq!(tally.weak_score, Some(0.7));
        let empty = Tally::of(&[]);
        assert_eq!((empty.weak_score, empty.mutation_score), (None, None));
        let by_language = tallies(&mutants, |mutant| language(&mutant.file));
        assert_eq!(
            by_language.keys().copied().collect::<Vec<_>>(),
            ["javascript", "python"]
        );
        assert_eq!(by_language["javascript"].mutation_score, Some(0.6667));
    }

    #[test]
    fn languages_are_named_as_the_report_schema_names_them() {
        let named = [
            "a.ts", "a.TSX", "a.mts", "a.cts", "a.py", "a.pyi", "a.rs", "a.go", "a.rb", "A.java",
            "a.kt", "a.kts", "a.js", "a.jsx", "a.mjs", "a.cjs", "a.c", "Makefile",
        ]
        .map(language);
        assert_eq!(
            named.join(","),
            "typescript,typescript,typescript,typescript,python,python,rust,go,ruby,java,kotlin,kotlin,javascript,javascript,javascript,javascript,other,other"
        );
    }

    #[test]
    fn a_stryker_report_maps_every_outcome_and_leaves_out_what_it_cannot_show() {
        use MutantStatus::*;
        let mut changed = entry(
            "src/a.js",
            2,
            Infected,
            Some(json!({"status": "killed", "killedBy": "t1", "tests": 2, "durationMs": 7})),
        );
        changed.changed = Some(MutantChange {
            from: ".x(1)".into(),
            to: "".into(),
        });
        let mutants = vec![
            changed,
            entry("src/a.js", 3, Infected, Some(json!({"status": "timeout"}))),
            entry("src/a.js", 4, Infected, Some(json!({"status": "survived"}))),
            entry(
                "src/a.js",
                5,
                NotReached,
                Some(json!({"status": "no-coverage"})),
            ),
            entry(
                "src/a.js",
                6,
                Equivalent,
                Some(json!({"status": "equivalent"})),
            ),
            entry("src/a.js", 7, Infected, Some(json!({"status": "unviable"}))),
            entry("src/a.js", 8, Infected, Some(json!({"status": "error"}))),
            entry(
                "src/a.js",
                9,
                Infected,
                Some(json!({"status": "error", "reason": "no strong mutation driver for go yet"})),
            ),
            entry("src/a.js", 10, NotReached, None),
            entry("src/a.js", 11, NotInfected, None),
            entry("src/a.js", 12, Equivalent, None),
            entry("src/a.js", 13, Infected, None),
            entry("src/a.js", 14, Unknown, None),
            entry("gone.rs", 1, Infected, None),
        ];
        let sources = BTreeMap::from([("src/a.js".to_owned(), "x\n".to_owned())]);
        let (report, missing) = stryker_report(&data(mutants), &sources);
        assert_eq!(missing, ["gone.rs"]);
        assert_eq!(report["schemaVersion"], "2");
        assert_eq!(report["framework"]["dependencies"]["mutantSet"], "set@1");
        let file = &report["files"]["src/a.js"];
        assert_eq!(file["language"], "javascript");
        let statuses = file["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|mutant| mutant["status"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            [
                "Killed",
                "Timeout",
                "Survived",
                "NoCoverage",
                "Ignored",
                "CompileError",
                "RuntimeError",
                "Pending",
                "NoCoverage",
                "Survived",
                "Ignored",
                "Pending",
                "Pending"
            ]
        );
        let killed = &file["mutants"][0];
        assert_eq!(killed["killedBy"], json!(["t1"]));
        assert_eq!(
            (killed["testsCompleted"].clone(), killed["duration"].clone()),
            (json!(2), json!(7))
        );
        assert_eq!(killed["description"], ".x(1) → ");
        assert_eq!(killed["location"]["start"], json!({"line": 2, "column": 5}));
        assert!(file["mutants"][1].get("killedBy").is_none());
        assert!(file["mutants"][9].get("testsCompleted").is_none());
        // No mutant set: an empty name.
        let mut unnamed = data(Vec::new());
        unnamed.mutant_set = None;
        assert_eq!(
            stryker_report(&unnamed, &sources).0["framework"]["dependencies"]["mutantSet"],
            ""
        );
    }

    #[test]
    fn pit_xml_names_each_mutant_as_pit_would_and_escapes_its_text() {
        use MutantStatus::*;
        let mut odd = entry(
            "src/main/java/com/shop/Cart.java",
            4,
            Infected,
            Some(json!({"status": "killed", "killedBy": "CartTest.<total>", "tests": 2})),
        );
        odd.original = "a & 'b' \"c\"\t\u{1}".into();
        let mutants = vec![
            odd,
            entry(
                "src/main/java/com/shop/Cart.java",
                4,
                Infected,
                Some(json!({"status": "timeout"})),
            ),
            entry("lib/x.py", 1, Infected, Some(json!({"status": "survived"}))),
            entry(
                "lib/x.py",
                2,
                Infected,
                Some(json!({"status": "no-coverage"})),
            ),
            entry("lib/x.py", 3, Infected, Some(json!({"status": "unviable"}))),
            entry("lib/x.py", 4, Infected, Some(json!({"status": "error"}))),
            entry(
                "lib/x.py",
                5,
                Infected,
                Some(
                    json!({"status": "error", "reason": "no strong mutation driver for python yet"}),
                ),
            ),
            entry(
                "lib/x.py",
                6,
                Equivalent,
                Some(json!({"status": "equivalent"})),
            ),
            entry("lib/x.py", 7, Equivalent, None),
            entry("lib/x.py", 8, NotReached, None),
            entry("lib/x.py", 9, NotInfected, None),
            entry("lib/x.py", 10, Infected, None),
            entry("lib/x.py", 11, Unknown, None),
            entry("app/src/main/kotlin/a/B.kt", 1, Infected, None),
            entry("Win\\Path.go", 1, Infected, None),
            entry("noext", 1, Infected, None),
        ];
        let xml = pit_xml(&data(mutants));
        let statuses = xml
            .lines()
            .filter_map(|line| line.split("status=\"").nth(1)?.split('"').next())
            .collect::<Vec<_>>();
        assert_eq!(
            statuses,
            [
                "KILLED",
                "TIMED_OUT",
                "SURVIVED",
                "NO_COVERAGE",
                "NON_VIABLE",
                "NOT_STARTED",
                "NOT_STARTED",
                "NO_COVERAGE",
                "SURVIVED",
                "NOT_STARTED",
                "NOT_STARTED",
                "NOT_STARTED",
                "NOT_STARTED",
                "NOT_STARTED"
            ]
        );
        assert!(xml.starts_with(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mutations partial=\"false\">\n"
        ));
        assert!(xml.ends_with("</mutations>\n"));
        let first = xml.lines().nth(2).unwrap();
        assert!(
            first.contains("detected=\"true\" status=\"KILLED\" numberOfTestsRun=\"2\""),
            "{first}"
        );
        assert!(
            first.contains(
                "<sourceFile>Cart.java</sourceFile><mutatedClass>com.shop.Cart</mutatedClass>"
            ),
            "{first}"
        );
        assert!(
            first.contains("<killingTest>CartTest.&lt;total&gt;</killingTest>"),
            "{first}"
        );
        assert!(
            first.contains("a &amp; &apos;b&apos; &quot;c&quot;  → a &lt; b"),
            "{first}"
        );
        assert!(first.contains("<index>0</index>"), "{first}");
        // The second mutant of the file is the file's second.
        assert!(xml.lines().nth(3).unwrap().contains("<index>1</index>"));
        assert!(xml.lines().nth(4).unwrap().contains("<index>0</index>"));
        assert!(xml.contains("<mutatedClass>lib.x</mutatedClass>"));
        assert!(xml.contains("<mutatedClass>a.B</mutatedClass>"));
        assert!(
            xml.contains("<sourceFile>Path.go</sourceFile><mutatedClass>Win.Path</mutatedClass>")
        );
        assert!(xml.contains("<sourceFile>noext</sourceFile><mutatedClass>noext</mutatedClass>"));
        // No mutants: an empty list.
        assert_eq!(
            pit_xml(&data(Vec::new())),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<mutations partial=\"false\">\n</mutations>\n"
        );
    }
}
