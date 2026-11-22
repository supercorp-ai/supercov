//! Strong mutation's plan (`docs/design/strong-mutation.md`): which of a
//! run's mutants need running, and against which of its tests. It reads
//! only what every language records -- each test's hits, and the values its
//! probes saw where the language records them -- and the run's mutant list
//! (`mutant_list`), so one plan serves every language's driver
//! (`docs/design/mutation-drivers.md`).
//!
//! A test can kill a mutant only if the mutant's value differed somewhere in
//! that test's own execution: until then, the test runs exactly as it ran
//! without the mutant. So a mutant's candidates are the tests whose own
//! evidence, read alone, leaves the mutant `infected` or `unknown` -- the
//! weak verdict applied one test at a time. Where a test's evidence is not
//! its own (its attribution is not exact), or the mutant's code ran in work
//! no test was credited with, the plan takes more tests, never fewer.
//!
//! Every exclusion here is a claim that a StrykerJS run would not see that
//! test kill that mutant. The strong pass is validated against StrykerJS run
//! with every killer recorded (`disableBail`), and a kill by a test this
//! plan left out is a soundness violation.

use std::collections::{BTreeMap, HashMap, HashSet};

use serde::Serialize;

use crate::coverage_report::{
    ATTRIBUTION_BY_TIME, ATTRIBUTION_EXACT, ArchiveReportRequest, RawTestResult, ReportError,
};
use crate::js_weak_mutation::{Observations, Status, observations, verdict};
use crate::mutant_list::{CandidateMutant, MutantsFile};

/// A test the plan may run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedTest {
    /// The test's id as its runner's hooks recorded it (`vitest:<id>`).
    pub id: String,
    /// Its file, relative to the project root.
    pub file: Option<String>,
    /// Its full name (`describe > it`).
    pub name: String,
    /// Its title alone.
    pub title: Option<String>,
    /// The runner it ran under.
    pub runner: String,
    /// How long it took in the measured run, when the runner said.
    pub duration_ms: Option<u64>,
    /// When its first attempt ran, on the runner's clock, where the runner
    /// said (Playwright): which tests the measured run ran at once.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<[i64; 2]>,
    /// Whether every attempt passed in the measured run: only such a test
    /// can be a candidate.
    pub runnable: bool,
}

/// Why a test is one of a mutant's candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateReason {
    /// Its own evidence shows the mutant's value differed.
    Infected,
    /// Its own evidence ran the code and cannot tell.
    Unknown,
    /// Its evidence is not its own (run-wide or partial attribution).
    NotExact,
    /// The mutant's code ran in work no test was credited with.
    Unattributed,
    /// The mutant's code ran while its file's modules loaded: every test of
    /// the file is a candidate, and the file runs whole.
    ModuleLoad,
}

/// One candidate test of a mutant: an index into the plan's tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Candidate {
    pub test: usize,
    pub reason: CandidateReason,
}

/// Why a mutant must be switched on in a whole server process, from its
/// start, rather than for the requests of one test's attempt
/// (`docs/design/strong-mutation.md`, browser tests against a server).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ServerWide {
    /// Its code ran in a server's work no request named: start-up, timers,
    /// work between tests.
    Background,
    /// Its code is a server module's top level, which runs once and keeps
    /// what it made for every later request.
    ModuleLevel,
    /// Its code ran for a request that named no test, credited to a test by
    /// the time it ran.
    ByTime,
}

/// What the plan says of a mutant without running it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Decided {
    /// A static rule shows no input can make it differ.
    Equivalent,
    /// No test ran its code.
    NoCoverage,
    /// No test infected it: none can kill it.
    Survived,
}

/// One listed mutant and what the strong pass does with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedMutant {
    pub id: String,
    pub file: String,
    /// Its file's language: which driver runs it.
    pub language: String,
    /// Its position in its file's list: the index its switch carries.
    pub index: usize,
    pub mutator: String,
    /// The weak status (`infected`, `unknown`, ...).
    pub weak: &'static str,
    /// Set where the plan decides it without running it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decided: Option<Decided>,
    /// The code of why it was decided so (the weak reason's code, or
    /// `no-passing-candidate`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Its candidates, cheapest first: tests with a duration before those
    /// without, then by name.
    pub candidates: Vec<Candidate>,
    /// Test files that must run whole for it (module-load candidates).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub whole_files: Vec<String>,
    /// Set where a server process ran its code in a way a request's own
    /// mutant cannot reach: such a mutant runs with the server started with
    /// it switched on. Drivers that run no server ignore it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_wide: Option<ServerWide>,
    /// How many tests ran its code: what a pass by coverage alone would run
    /// it against. The candidates are those of them that infected it.
    pub covering: usize,
    /// The statement and function points its reach is read from, for
    /// whoever checks a plan.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub statement: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// How many records of each role ran its function point.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub function_hit_by: BTreeMap<String, usize>,
}

/// The plan for a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrongPlan {
    pub mutant_set: String,
    pub tests: Vec<PlannedTest>,
    pub mutants: Vec<PlannedMutant>,
}

/// The file in a run's directory that holds its strong verdicts.
pub const STRONG_VERDICTS_FILE: &str = "strong-mutation.json";

/// The version of [`STRONG_VERDICTS_FILE`].
pub const STRONG_VERDICTS_SCHEMA_VERSION: u32 = 1;

/// A run's strong verdicts, as `mutants test` saves them into the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct StrongVerdicts {
    pub schema_version: u32,
    pub run: String,
    pub mutant_set: String,
    /// Each mutant's verdict, by its id.
    pub mutants: BTreeMap<String, Verdict>,
}

/// One mutant's strong verdict, as saved, and as a driver reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub struct Verdict {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub killed_by: Option<String>,
    #[serde(default)]
    pub tests: u64,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A stored run's plan, and the files whose mutants could not be listed.
pub struct ArchivePlan {
    /// `None` when the run has no mutants and none could be listed.
    pub plan: Option<StrongPlan>,
    /// Files changed since the run: their mutants are left out until the
    /// run is measured again.
    pub changed: Vec<String>,
}

/// The plan for a stored run's archive. A run measured without its mutants
/// (a plain coverage run) has them listed from the sources under `root`, as
/// they were measured, and kept beside the archive for every later query.
pub fn plan_for_archive(
    request: &ArchiveReportRequest,
    root: &std::path::Path,
) -> Result<ArchivePlan, ReportError> {
    let (raw_results, listed, points) = crate::coverage_report::archive_tests_and_mutants(request)?;
    let (listed, changed) = listed_or_list(request, root, listed, &points)?;
    Ok(ArchivePlan {
        plan: listed.and_then(|listed| plan(&raw_results, listed)),
        changed,
    })
}

/// Lists a run's mutants from its sources under `root`, unless the run or
/// an earlier ask listed them: what a query about its mutants does first.
/// The files changed since the run, whose mutants could not be listed.
pub fn list_run_mutants(
    request: &ArchiveReportRequest,
    root: &std::path::Path,
) -> Result<Vec<String>, ReportError> {
    if beside(request).exists() {
        return Ok(Vec::new());
    }
    let (_, listed, points) = crate::coverage_report::archive_tests_and_mutants(request)?;
    Ok(listed_or_list(request, root, listed, &points)?.1)
}

/// Whether the run at `archive_path` listed its mutants as it measured, and
/// so recorded the values their probes see: a run `supercov mutants` can
/// plan from as it is. A plain run's mutants, listed after it, have no
/// values, and their plan runs more tests.
pub fn run_recorded_mutant_values(archive_path: &std::path::Path) -> bool {
    let listed_after = archive_path.with_file_name(crate::coverage_report::LISTED_MUTANTS_FILE);
    if let Ok(bytes) = std::fs::read(&listed_after)
        && let Ok(listed) = serde_json::from_slice::<MutantsFile>(&bytes)
    {
        // An empty list there marks a run that had its own.
        return listed.mutant_set.is_none();
    }
    let mut listed = false;
    let _ = crate::evidence_archive::read_archive_each(
        archive_path,
        |path| path == "mutants.json",
        |entry| {
            listed = serde_json::from_slice::<MutantsFile>(&entry.contents)
                .is_ok_and(|mutants| mutants.mutant_set.is_some());
            Ok(())
        },
    );
    listed
}

fn beside(request: &ArchiveReportRequest) -> std::path::PathBuf {
    request
        .archive_path
        .with_file_name(crate::coverage_report::LISTED_MUTANTS_FILE)
}

/// The run's own mutants where it listed them, else those listed from its
/// sources, written beside its archive. Either way a file is left there, so
/// a later query knows without reading the archive that nothing is left to
/// list: an empty list where the run had its own.
fn listed_or_list(
    request: &ArchiveReportRequest,
    root: &std::path::Path,
    listed: Option<MutantsFile>,
    points: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> Result<(Option<MutantsFile>, Vec<String>), ReportError> {
    let path = beside(request);
    let (kept, written, changed) = match listed {
        Some(own) if own.mutant_set.is_some() => {
            let none = MutantsFile {
                schema_version: own.schema_version,
                ..MutantsFile::unlisted()
            };
            (Some(own), none, Vec::new())
        }
        _ if points.is_empty() => return Ok((None, Vec::new())),
        _ => {
            let fresh = crate::mutant_list::list_from_sources(root, points);
            (Some(fresh.mutants.clone()), fresh.mutants, fresh.changed)
        }
    };
    if !path.exists() {
        // Written whole, then renamed into place: a query reading it
        // meanwhile finds no file or the whole one.
        let partial = path.with_extension("json.partial");
        std::fs::write(
            &partial,
            serde_json::to_vec(&written).expect("plain data serializes"),
        )
        .and_then(|()| std::fs::rename(&partial, &path))
        .map_err(|error| ReportError::InvalidArchive(format!("{}: {error}", path.display())))?;
    }
    Ok((kept, changed))
}

/// A record's role: one a test's, a module setup's, or the run's
/// background.
fn role_of(raw: &RawTestResult) -> &str {
    raw.role.as_str()
}

fn passed(raw: &RawTestResult) -> bool {
    raw.status.as_deref() == Some("passed") && !raw.flaky
}

fn exact(raw: &RawTestResult) -> bool {
    raw.attribution == ATTRIBUTION_EXACT || raw.attribution == ATTRIBUTION_BY_TIME
}

/// One record's own observations, with the run's notes on records that
/// carry no values applied to it.
fn own_observations(raw: &RawTestResult, without_values: &HashSet<&String>) -> Observations {
    let mut seen = observations(std::slice::from_ref(raw));
    if raw
        .test_id
        .as_ref()
        .is_some_and(|id| without_values.contains(id))
    {
        seen.unvalued_hits = seen.hits.clone();
    }
    seen
}

/// `from`'s observations added to `into`'s.
fn merge(into: &mut Observations, from: &Observations) {
    into.hits.extend(from.hits.iter().cloned());
    into.server_hits.extend(from.server_hits.iter().cloned());
    into.unvalued_hits
        .extend(from.unvalued_hits.iter().cloned());
    for (id, vectors) in &from.decisions {
        let seen = into.decisions.entry(id.clone()).or_default();
        for vector in vectors {
            if !seen.contains(vector) {
                seen.push(vector.clone());
            }
        }
    }
    for (id, outcomes) in &from.logicals {
        into.logicals
            .entry(id.clone())
            .or_default()
            .extend(outcomes.iter().copied());
    }
    for (id, outcomes) in &from.comparisons {
        into.comparisons
            .entry(id.clone())
            .or_default()
            .extend(outcomes.iter().cloned());
    }
}

/// Whether the mutant's code is among what `seen` hit: its statement, or
/// its function where no statement contains it. A mutant with neither
/// point cannot be narrowed, and counts as hit by anything.
fn may_reach(mutant: &CandidateMutant, seen: &Observations) -> bool {
    mutant
        .statement
        .as_ref()
        .is_none_or(|statement| seen.hits.contains(statement))
        && mutant
            .function
            .as_ref()
            .is_none_or(|function| seen.hits.contains(function))
}

/// Whether `seen` could have infected the mutant: its verdict read from
/// `seen` alone is `infected` or `unknown`.
fn could_infect(
    mutant: &CandidateMutant,
    seen: &Observations,
    whole: &Observations,
) -> Option<CandidateReason> {
    // A method's body is reached when its function runs, though the
    // statement around the function (the export of the object it is on)
    // ran once, when the module loaded, in whichever test loaded it first.
    // Where this test ran the function and the run ran the statement, the
    // statement counts as run here: it only ever adds a candidate.
    let widened;
    let seen = match (&mutant.statement, &mutant.function) {
        (Some(statement), Some(function))
            if seen.hits.contains(function)
                && !seen.hits.contains(statement)
                && whole.hits.contains(statement) =>
        {
            let mut copy = seen.clone();
            copy.hits.insert(statement.clone());
            widened = copy;
            &widened
        }
        _ => seen,
    };
    if !may_reach(mutant, seen) {
        return None;
    }
    match verdict(mutant, seen).0 {
        Status::Infected { .. } => Some(CandidateReason::Infected),
        Status::Unknown => Some(CandidateReason::Unknown),
        Status::NotReached | Status::NotInfected | Status::Equivalent => None,
    }
}

/// Whether a server process ran the mutant's code in this record in a way
/// a request's own mutant cannot reach, and how.
fn server_wide(raw: &RawTestResult, mutant: &CandidateMutant) -> Option<ServerWide> {
    let points = [&mutant.statement, &mutant.function]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut ran = raw.server.iter().filter(|record| {
        record.record_type == "hit" && record.id.as_ref().is_some_and(|id| points.contains(&id))
    });
    let first = ran.next()?;
    if !matches!(role_of(raw), "test" | "setup") {
        // Work for a request that named no test and ran while none did (a
        // web server's readiness check) ran without any mutant in the pass,
        // as each request runs with its own; only work no request started
        // (start-up, timers) needs the server started with the mutant.
        if std::iter::once(first)
            .chain(ran)
            .any(|record| record.request.is_none())
        {
            return Some(ServerWide::Background);
        }
        return mutant.function.is_none().then_some(ServerWide::ModuleLevel);
    }
    if mutant.function.is_none() {
        return Some(ServerWide::ModuleLevel);
    }
    std::iter::once(first)
        .chain(ran)
        .any(|record| record.request.is_some())
        .then_some(ServerWide::ByTime)
}

fn weak_name(status: Status) -> &'static str {
    match status {
        Status::NotReached => "not-reached",
        Status::NotInfected => "not-infected",
        Status::Infected { .. } => "infected",
        Status::Unknown => "unknown",
        Status::Equivalent => "equivalent",
    }
}

/// The plan: every listed mutant, with what the run decides of it or the
/// tests it must run against. `None` when the run listed no mutant set.
pub(crate) fn plan(raw_results: &[RawTestResult], listed: MutantsFile) -> Option<StrongPlan> {
    let mutant_set = listed.mutant_set.clone()?;
    let without_values = listed.tests_without_values.iter().collect::<HashSet<_>>();

    // The run as a whole: the weak verdict every mutant has.
    let mut whole = observations(raw_results);
    if !without_values.is_empty() {
        for raw in raw_results.iter().filter(|raw| {
            raw.test_id
                .as_ref()
                .is_some_and(|id| without_values.contains(id))
        }) {
            let hits = observations(std::slice::from_ref(raw)).hits;
            whole.unvalued_hits.extend(hits);
        }
    }

    // Tests, one per test id: a test's attempts (retries) are one test, and
    // it is runnable only where every attempt passed.
    let mut tests = Vec::<PlannedTest>::new();
    let mut test_index = HashMap::<String, usize>::new();
    let mut runnable = Vec::<bool>::new();
    // Whether any record of the test said how it ended: Jest writes a test's
    // evidence (`unknown`) and its outcome in two records.
    let mut stated = Vec::<bool>::new();
    let mut by_file = BTreeMap::<String, Vec<usize>>::new();
    for raw in raw_results.iter().filter(|raw| role_of(raw) == "test") {
        let id = raw.test_id.clone().unwrap_or_else(|| raw.test.clone());
        let index = *test_index.entry(id.clone()).or_insert_with(|| {
            tests.push(PlannedTest {
                id: id.clone(),
                file: raw.test_file.clone(),
                name: raw.test.clone(),
                title: raw.title.clone(),
                runner: raw.provenance.runner.clone(),
                duration_ms: None,
                span: raw.span,
                runnable: true,
            });
            runnable.push(true);
            stated.push(false);
            if let Some(file) = &raw.test_file {
                by_file
                    .entry(file.clone())
                    .or_default()
                    .push(tests.len() - 1);
            }
            tests.len() - 1
        });
        if raw.status.as_deref() == Some("unknown") && !raw.flaky {
            // Its evidence alone: the outcome is another record's.
        } else {
            runnable[index] &= passed(raw);
            stated[index] = true;
        }
        // An attempt has two records under Playwright, the reporter's (its
        // status, no span) and the fixture's (its evidence, its span), in
        // either order: the span is the first attempt's, whichever record
        // carries it.
        if let Some(span) = raw.span
            && tests[index].span.is_none_or(|seen| span[0] < seen[0])
        {
            tests[index].span = Some(span);
        }
        // A runner that says no duration (Playwright) gives the attempt's
        // first and last moment.
        let measured = raw.duration_ms.or_else(|| {
            raw.span
                .and_then(|[start, end]| u64::try_from(end.saturating_sub(start)).ok())
        });
        if let Some(ms) = measured {
            let duration = &mut tests[index].duration_ms;
            *duration = Some(duration.map_or(ms, |seen| seen.max(ms)));
        }
    }

    // Each record's own observations, and an index from a hit point to the
    // records that hit it, so a mutant's verdict is read only from the
    // records that ran its code.
    struct Seen {
        record: usize,
        seen: Observations,
    }
    for ((test, ok), stated) in tests.iter_mut().zip(&mut runnable).zip(&stated) {
        *ok &= *stated;
        test.runnable = *ok;
    }
    let mut seen = Vec::<Seen>::new();
    let mut by_point = HashMap::<String, Vec<usize>>::new();
    let mut unbounded = Vec::<usize>::new();
    // What each test file's modules did as they loaded: every test of the
    // file runs after it, so a test's own evidence is read with it. A
    // method of an object a module exports is reached when its statement
    // (the export) ran, at load, and its function, in the test.
    let mut loaded = HashMap::<&str, Observations>::new();
    for raw in raw_results.iter().filter(|raw| role_of(raw) == "setup") {
        if let Some(file) = &raw.test_file {
            let own = own_observations(raw, &without_values);
            merge(loaded.entry(file.as_str()).or_default(), &own);
        }
    }
    for (record, raw) in raw_results.iter().enumerate() {
        let mut own = own_observations(raw, &without_values);
        if role_of(raw) == "test"
            && let Some(at_load) = raw.test_file.as_deref().and_then(|file| loaded.get(file))
        {
            merge(&mut own, at_load);
        }
        let at = seen.len();
        if role_of(raw) == "test" && !exact(raw) {
            unbounded.push(at);
        }
        for point in &own.hits {
            by_point.entry(point.clone()).or_default().push(at);
        }
        seen.push(Seen { record, seen: own });
    }

    let mut mutants = Vec::new();
    for (file, listed_mutants) in &listed.files {
        for (index, mutant) in listed_mutants.iter().enumerate() {
            let (status, reason) = verdict(mutant, &whole);
            let mut planned = PlannedMutant {
                id: if mutant.id.is_empty() {
                    format!(
                        "{file}:{}:{}-{}:{}:{}:{}",
                        mutant.start.0,
                        mutant.start.1,
                        mutant.end.0,
                        mutant.end.1,
                        mutant.mutator,
                        mutant.replacement
                    )
                } else {
                    mutant.id.clone()
                },
                file: file.clone(),
                language: listed.language_of(file).to_owned(),
                index,
                mutator: mutant.mutator.clone(),
                weak: weak_name(status),
                decided: None,
                reason: None,
                candidates: Vec::new(),
                whole_files: Vec::new(),
                server_wide: None,
                covering: 0,
                statement: mutant.statement.clone(),
                function: mutant.function.clone(),
                function_hit_by: BTreeMap::new(),
            };
            if let Some(function) = &mutant.function {
                for at in by_point.get(function).into_iter().flatten() {
                    let role = role_of(&raw_results[seen[*at].record]).to_owned();
                    *planned.function_hit_by.entry(role).or_default() += 1;
                }
            }
            match status {
                Status::Equivalent => {
                    planned.decided = Some(Decided::Equivalent);
                    planned.reason = Some(reason.code.to_owned());
                }
                Status::NotReached => {
                    planned.decided = Some(Decided::NoCoverage);
                    planned.reason = Some(reason.code.to_owned());
                }
                // Not infected where the operands are clock or random
                // readings proves only that this run's values never met:
                // another run's may. Such a mutant runs against the tests
                // that reached it.
                Status::NotInfected if mutant.untestable.is_none() => {
                    planned.decided = Some(Decided::Survived);
                    planned.reason = Some(reason.code.to_owned());
                }
                Status::Infected { .. } | Status::Unknown | Status::NotInfected => {
                    let reach_only = status == Status::NotInfected;
                    let mut chosen = BTreeMap::<usize, CandidateReason>::new();
                    let mut whole_files = HashSet::<String>::new();
                    let mut take = |test: usize, why: CandidateReason| {
                        chosen.entry(test).or_insert(why);
                    };
                    // The records that ran the mutant's point (or all, for a
                    // mutant no point contains), and every record whose
                    // silence proves nothing.
                    // Reach needs both points; the function's is the narrower.
                    let point = mutant.function.as_ref().or(mutant.statement.as_ref());
                    let mut records = match point {
                        Some(point) => by_point.get(point).cloned().unwrap_or_default(),
                        None => (0..seen.len()).collect(),
                    };
                    records.extend(unbounded.iter().copied());
                    records.sort_unstable();
                    records.dedup();
                    let mut covering = HashSet::<usize>::new();
                    for at in records {
                        let Seen { record, seen: own } = &seen[at];
                        let raw = &raw_results[*record];
                        // Every test record was indexed by this id above.
                        if role_of(raw) == "test"
                            && (may_reach(mutant, own)
                                || mutant
                                    .function
                                    .as_ref()
                                    .is_some_and(|function| own.hits.contains(function)))
                        {
                            covering.insert(
                                test_index
                                    [&raw.test_id.clone().unwrap_or_else(|| raw.test.clone())],
                            );
                        }
                        let why = if role_of(raw) == "test" && !exact(raw) {
                            Some(CandidateReason::NotExact)
                        } else if reach_only {
                            // Every record read here ran the mutant's
                            // point, or the mutant has none, but an inexact
                            // test's, which was taken above.
                            Some(CandidateReason::Unknown)
                        } else {
                            could_infect(mutant, own, &whole)
                        };
                        let Some(why) = why else { continue };
                        if planned.server_wide.is_none() {
                            planned.server_wide = server_wide(raw, mutant);
                        }
                        match role_of(raw) {
                            "test" => {
                                let id = raw.test_id.clone().unwrap_or_else(|| raw.test.clone());
                                take(test_index[&id], why);
                                // Module-level code a test ran (a module it
                                // loaded first): the module keeps what it
                                // made for every later test of the file, which
                                // runs none of that code again.
                                if mutant.function.is_none()
                                    && let Some(file) = &raw.test_file
                                {
                                    whole_files.insert(file.clone());
                                    for &test in by_file.get(file).into_iter().flatten() {
                                        take(test, CandidateReason::ModuleLoad);
                                    }
                                }
                            }
                            // A file's module setup: every test of the file.
                            "setup" => {
                                if let Some(file) = &raw.test_file {
                                    whole_files.insert(file.clone());
                                    for &test in by_file.get(file).into_iter().flatten() {
                                        take(test, CandidateReason::ModuleLoad);
                                    }
                                }
                            }
                            // Work no test was credited with: any test of the
                            // run may have caused it.
                            _ => {
                                for test in 0..tests.len() {
                                    take(test, CandidateReason::Unattributed);
                                }
                            }
                        }
                    }
                    let mut candidates = chosen
                        .into_iter()
                        .filter(|(test, _)| runnable[*test])
                        .map(|(test, reason)| Candidate { test, reason })
                        .collect::<Vec<_>>();
                    candidates.sort_by(|a, b| {
                        let (ta, tb) = (&tests[a.test], &tests[b.test]);
                        (ta.duration_ms.is_none(), ta.duration_ms, &ta.name).cmp(&(
                            tb.duration_ms.is_none(),
                            tb.duration_ms,
                            &tb.name,
                        ))
                    });
                    if candidates.is_empty() {
                        planned.decided = Some(Decided::Survived);
                        planned.reason = Some("no-passing-candidate".into());
                    }
                    let mut whole_files = whole_files.into_iter().collect::<Vec<_>>();
                    whole_files.sort();
                    planned.candidates = candidates;
                    planned.whole_files = whole_files;
                    planned.covering = covering.len();
                }
            }
            mutants.push(planned);
        }
    }
    Some(StrongPlan {
        mutant_set,
        tests,
        mutants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coverage_analysis::McdcVector;
    use crate::coverage_report::{
        ATTRIBUTION_RUN_WIDE, ComparisonSnapshot, DecisionSnapshot, ExitCodeInput, LogicalSnapshot,
        LogicalVector, RuntimeSnapshot, ServerRecord,
    };
    use crate::evidence_archive::{EvidenceArchiveEntry, write_archive};

    /// A passed record of `role` that hit `hits`, under the id `id`.
    fn record(id: &str, role: &str, file: Option<&str>, hits: &[&str]) -> RawTestResult {
        let mut raw: RawTestResult = serde_json::from_value(serde_json::json!({
            "testId": id, "test": id, "status": "passed", "role": role,
            "provenance": {"runner": "vitest", "kind": "unit", "source": "s"}
        }))
        .unwrap();
        raw.test_file = file.map(str::to_owned);
        raw.runtime = vec![RuntimeSnapshot {
            hits: hits.iter().map(|hit| (*hit).to_owned()).collect(),
            ..RuntimeSnapshot::default()
        }];
        raw
    }

    fn decision(id: &str, vectors: &[(bool, bool)]) -> DecisionSnapshot {
        DecisionSnapshot {
            id: id.into(),
            vectors: vectors
                .iter()
                .map(|&(value, outcome)| McdcVector {
                    values: vec![Some(value)],
                    outcome,
                })
                .collect(),
        }
    }

    fn server_hit(id: &str, request: Option<u64>) -> ServerRecord {
        ServerRecord {
            record_type: "hit".into(),
            vector: None,
            id: Some(id.into()),
            timestamp_ms: None,
            phase_id: None,
            scope: None,
            request,
            outcome: None,
        }
    }

    /// A `BooleanLiteral` mutant of `true` at 1:1-1:5, with `fields` over it.
    fn mutant(fields: serde_json::Value) -> CandidateMutant {
        let mut all = serde_json::json!({
            "mutator": "BooleanLiteral", "replacement": "false",
            "start": [1, 1], "end": [1, 5], "original": "true"
        });
        all.as_object_mut()
            .unwrap()
            .extend(fields.as_object().unwrap().clone());
        serde_json::from_value(all).unwrap()
    }

    fn listed(mutants: Vec<CandidateMutant>, without_values: &[&str]) -> MutantsFile {
        MutantsFile {
            schema_version: 1,
            mutant_set: Some("set".into()),
            mutant_sets: Default::default(),
            languages: Default::default(),
            files: if mutants.is_empty() {
                BTreeMap::new()
            } else {
                BTreeMap::from([("src/app.js".to_owned(), mutants)])
            },
            tests_without_values: without_values.iter().map(|id| (*id).to_owned()).collect(),
        }
    }

    /// Each mutant's candidates as the names of their tests and why.
    fn candidates(plan: &StrongPlan, id: &str) -> Vec<(String, CandidateReason)> {
        plan.mutants
            .iter()
            .find(|mutant| mutant.id == id)
            .unwrap()
            .candidates
            .iter()
            .map(|candidate| (plan.tests[candidate.test].name.clone(), candidate.reason))
            .collect()
    }

    fn planned<'p>(plan: &'p StrongPlan, id: &str) -> &'p PlannedMutant {
        plan.mutants.iter().find(|mutant| mutant.id == id).unwrap()
    }

    #[test]
    fn a_run_that_listed_no_mutant_set_has_no_plan() {
        let mut none = listed(Vec::new(), &[]);
        none.mutant_set = None;
        assert_eq!(plan(&[record("a", "test", None, &[])], none), None);
    }

    /// A run with no records and a file of no mutants plans nothing, though
    /// it names tests measured without values that none of its records is.
    #[test]
    fn a_run_without_records_or_mutants_plans_nothing_to_run() {
        let mut empty = listed(Vec::new(), &["gone"]);
        empty.files.insert("src/empty.js".into(), Vec::new());
        let plan = plan(&[], empty).unwrap();
        assert_eq!(plan.mutant_set, "set");
        assert!(plan.tests.is_empty());
        assert!(plan.mutants.is_empty());
    }

    /// A test's attempts are one test: it is runnable only where every
    /// attempt passed and none was flaky, its span is its earliest
    /// attempt's, and its duration its longest attempt's, read from the
    /// span where the runner said none.
    #[test]
    fn a_test_s_attempts_are_one_test_runnable_only_where_every_attempt_passed() {
        let spanned = |span: [i64; 2]| RawTestResult {
            span: Some(span),
            ..record("a", "test", Some("t/a.test.js"), &[])
        };
        let mut failed = record("b", "test", Some("t/b.test.js"), &[]);
        failed.status = Some("failed".into());
        let mut flaky = record("c", "test", Some("t/c.test.js"), &[]);
        flaky.flaky = true;
        let raw_results = [
            spanned([10, 20]),
            spanned([15, 40]),
            spanned([5, 8]),
            failed,
            flaky,
            record("d", "test", None, &[]),
        ];
        let plan = plan(&raw_results, listed(Vec::new(), &[])).unwrap();
        let tests = plan
            .tests
            .iter()
            .map(|test| {
                (
                    test.id.as_str(),
                    test.file.as_deref(),
                    test.runnable,
                    test.span,
                    test.duration_ms,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            tests,
            [
                ("a", Some("t/a.test.js"), true, Some([5, 8]), Some(25)),
                ("b", Some("t/b.test.js"), false, None, None),
                ("c", Some("t/c.test.js"), false, None, None),
                ("d", None, true, None, None),
            ]
        );
        assert!(plan.mutants.is_empty());
    }

    /// Each mutant's candidates are the tests whose own evidence, with what
    /// their file's modules did as they loaded, may infect it: module-level
    /// code takes every test of the file it ran in, a setup no file owns
    /// takes none, and a mutant no passing test may infect survives.
    #[test]
    fn a_mutant_s_candidates_are_the_tests_whose_own_evidence_may_infect_it() {
        let mut load = record("load", "setup", Some("t/a.test.js"), &["s-load"]);
        load.runtime[0].decisions = vec![decision("d-load", &[(true, true), (false, false)])];
        load.runtime[0].logicals = vec![LogicalSnapshot {
            id: "b".into(),
            vectors: vec![LogicalVector {
                right: true,
                truthy: false,
            }],
        }];
        load.runtime[0].comparisons = vec![ComparisonSnapshot {
            id: "c".into(),
            outcomes: vec!["lt".into()],
        }];
        let mut a = record("a", "test", Some("t/a.test.js"), &["s", "f"]);
        a.runtime[0].decisions = vec![
            decision("d-load", &[(true, true)]),
            decision("d", &[(true, true)]),
        ];
        // Its server's work for a request that named no test, credited to
        // it by time.
        a.server = vec![server_hit("f", Some(1))];
        let mut by_time = record("t", "test", Some("t/t.test.js"), &["s-time"]);
        by_time.attribution = ATTRIBUTION_BY_TIME.into();
        let mut named = record("named only", "test", Some("t/n.test.js"), &["s", "f"]);
        named.test_id = None;
        named.runtime[0].decisions = vec![decision("d", &[(true, true)])];
        let raw_results = [
            load,
            record("loose", "setup", None, &["s-setup"]),
            a,
            by_time,
            named,
            record("u", "test", Some("t/u.test.js"), &["s-unvalued"]),
            record("nf", "test", None, &["s-nf"]),
            // The same test again, from a file no test was first seen in.
            record("a", "test", Some("t/other.test.js"), &["s-other"]),
            record("nobody", "setup", Some("t/nobody.test.js"), &["s-nobody"]),
        ];
        let mutants = vec![
            mutant(serde_json::json!({"id": "fn", "statement": "s", "function": "f"})),
            mutant(serde_json::json!({
                "statement": "s",
                "equivalent": {"rule": "some-rule", "reason": "no input differs"}
            })),
            mutant(serde_json::json!({
                "id": "clock", "mutator": "ConditionalExpression", "replacement": "true",
                "original": "a > b", "statement": "s", "function": "f",
                "decision": {"id": "d"},
                "untestable": {"rule": "clock", "reason": "reads the clock"}
            })),
            mutant(serde_json::json!({"id": "module", "statement": "s-load"})),
            mutant(serde_json::json!({"id": "loose", "statement": "s-setup"})),
            mutant(serde_json::json!({
                "id": "unvalued", "mutator": "EqualityOperator", "replacement": "<=",
                "original": "a < b", "operator": {"text": "<", "at": 2},
                "statement": "s-unvalued", "compare": "cmp-u"
            })),
            mutant(serde_json::json!({"id": "nofile", "statement": "s-nf"})),
            mutant(serde_json::json!({"id": "other", "statement": "s-other"})),
            mutant(serde_json::json!({"id": "nobody", "statement": "s-nobody"})),
        ];
        let plan = plan(&raw_results, listed(mutants, &["u"])).unwrap();
        let names = plan
            .tests
            .iter()
            .map(|test| test.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, ["a", "t", "named only", "u", "nf"]);
        let both = |reason| vec![("a".to_owned(), reason), ("named only".to_owned(), reason)];

        let infected = planned(&plan, "fn");
        assert_eq!(candidates(&plan, "fn"), both(CandidateReason::Infected));
        assert_eq!(infected.covering, 2);
        assert_eq!(infected.server_wide, Some(ServerWide::ByTime));
        assert_eq!(
            infected.function_hit_by,
            BTreeMap::from([("test".to_owned(), 2)])
        );

        // An equivalent mutant stored without its id has the one its place
        // makes.
        let equivalent = planned(&plan, "src/app.js:1:1-1:5:BooleanLiteral:false");
        assert_eq!(
            (
                equivalent.weak,
                equivalent.decided,
                equivalent.reason.as_deref()
            ),
            ("equivalent", Some(Decided::Equivalent), Some("static"))
        );
        assert!(equivalent.candidates.is_empty());

        // Not infected only where the clock's readings never met: every
        // test that reached it runs it.
        let clock = planned(&plan, "clock");
        assert_eq!((clock.weak, clock.decided), ("not-infected", None));
        assert_eq!(candidates(&plan, "clock"), both(CandidateReason::Unknown));
        assert_eq!(clock.server_wide, Some(ServerWide::ByTime));

        let module = planned(&plan, "module");
        assert_eq!(
            candidates(&plan, "module"),
            [("a".to_owned(), CandidateReason::ModuleLoad)]
        );
        assert_eq!(module.whole_files, ["t/a.test.js"]);
        assert_eq!(module.covering, 1);

        for survivor in ["loose", "nobody"] {
            let survived = planned(&plan, survivor);
            assert_eq!(
                (survived.decided, survived.reason.as_deref()),
                (Some(Decided::Survived), Some("no-passing-candidate")),
                "{survivor}"
            );
            assert!(survived.candidates.is_empty(), "{survivor}");
        }
        assert!(planned(&plan, "loose").whole_files.is_empty());
        assert_eq!(planned(&plan, "nobody").whole_files, ["t/nobody.test.js"]);

        // A test measured without the probes ran the comparison and
        // recorded no value of it: it cannot tell, so it runs.
        assert_eq!(planned(&plan, "unvalued").weak, "unknown");
        assert_eq!(
            candidates(&plan, "unvalued"),
            [("u".to_owned(), CandidateReason::Unknown)]
        );
        assert_eq!(planned(&plan, "unvalued").whole_files, ["t/u.test.js"]);

        assert_eq!(
            candidates(&plan, "nofile"),
            [("nf".to_owned(), CandidateReason::Infected)]
        );
        assert!(planned(&plan, "nofile").whole_files.is_empty());
        assert_eq!(
            candidates(&plan, "other"),
            [("a".to_owned(), CandidateReason::Infected)]
        );
        assert_eq!(planned(&plan, "other").whole_files, ["t/other.test.js"]);
    }

    /// A test whose evidence is not its own is a candidate of every mutant
    /// that runs, though it did not run the mutant's function, and does not
    /// count as covering it.
    #[test]
    fn a_test_whose_evidence_is_not_its_own_is_a_candidate_it_did_not_cover() {
        let mut wide = record("e", "test", Some("t/e.test.js"), &["s"]);
        wide.attribution = ATTRIBUTION_RUN_WIDE.into();
        let raw_results = [record("a", "test", Some("t/a.test.js"), &["s", "f"]), wide];
        let mutants = vec![mutant(
            serde_json::json!({"id": "fn", "statement": "s", "function": "f"}),
        )];
        let plan = plan(&raw_results, listed(mutants, &[])).unwrap();
        assert_eq!(
            candidates(&plan, "fn"),
            [
                ("a".to_owned(), CandidateReason::Infected),
                ("e".to_owned(), CandidateReason::NotExact),
            ]
        );
        assert_eq!(planned(&plan, "fn").covering, 1);
    }

    /// Work no test was credited with makes every test of the run a
    /// candidate: a run of none has no candidate, and each such mutant
    /// survives, as does one whose function no record ran.
    #[test]
    fn a_mutant_only_uncredited_work_ran_survives_a_run_without_tests() {
        let raw_results = [record("bg", "background", None, &["s"])];
        let mutants = vec![
            mutant(serde_json::json!({"id": "bg", "statement": "s"})),
            mutant(serde_json::json!({"id": "nopoint"})),
            mutant(serde_json::json!({"id": "unhit", "function": "never"})),
        ];
        let plan = plan(&raw_results, listed(mutants, &[])).unwrap();
        assert!(plan.tests.is_empty());
        let outcomes = plan
            .mutants
            .iter()
            .map(|mutant| {
                (
                    mutant.id.as_str(),
                    mutant.weak,
                    mutant.decided,
                    mutant.reason.as_deref(),
                    mutant.candidates.len(),
                )
            })
            .collect::<Vec<_>>();
        let survived = Some(Decided::Survived);
        let reason = Some("no-passing-candidate");
        assert_eq!(
            outcomes,
            [
                ("bg", "infected", survived, reason, 0),
                ("nopoint", "unknown", survived, reason, 0),
                ("unhit", "unknown", survived, reason, 0),
            ]
        );
    }

    /// Merging observations keeps each decision's evaluations once, a
    /// decision evaluated nowhere among them, and every logical and
    /// comparison outcome.
    #[test]
    fn merged_observations_keep_each_evaluation_once() {
        let mut into = Observations::default();
        into.decisions
            .insert("d".into(), vec![(vec![Some(true)], true)]);
        let mut from = Observations::default();
        from.decisions.insert(
            "d".into(),
            vec![(vec![Some(true)], true), (vec![Some(false)], false)],
        );
        from.decisions.insert("none".into(), Vec::new());
        from.logicals
            .insert("b".into(), HashSet::from([(true, false)]));
        from.comparisons
            .insert("c".into(), HashSet::from(["lt".to_owned()]));
        merge(&mut into, &from);
        assert_eq!(
            into.decisions["d"],
            [(vec![Some(true)], true), (vec![Some(false)], false)]
        );
        assert!(into.decisions["none"].is_empty());
        assert_eq!(into.logicals["b"], HashSet::from([(true, false)]));
        assert_eq!(into.comparisons["c"], HashSet::from(["lt".to_owned()]));
    }

    /// A record that did not run the mutant's function cannot infect it,
    /// and neither can one that ran only its function where the run never
    /// ran its statement: the statement counts as run only where the run
    /// ran it.
    #[test]
    fn a_record_infects_a_method_only_where_it_ran_the_function_and_the_run_the_statement() {
        let method = mutant(serde_json::json!({"statement": "s", "function": "f"}));
        let hits = |hits: &[&str]| Observations {
            hits: hits.iter().map(|hit| (*hit).to_owned()).collect(),
            ..Observations::default()
        };
        assert!(!may_reach(&method, &hits(&["s"])));
        assert_eq!(
            could_infect(&method, &hits(&["s"]), &hits(&["s", "f"])),
            None
        );
        assert_eq!(could_infect(&method, &hits(&["f"]), &hits(&["f"])), None);
        assert_eq!(
            could_infect(&method, &hits(&["f"]), &hits(&["s", "f"])),
            Some(CandidateReason::Infected)
        );
    }

    /// Module-level code a server ran for a test is the server module's top
    /// level, which runs once for every later request.
    #[test]
    fn module_level_code_a_server_ran_for_a_test_needs_the_server_started_with_it() {
        let mut raw = record("a", "test", None, &[]);
        raw.server = vec![server_hit("s", Some(1))];
        let module_level = mutant(serde_json::json!({"statement": "s"}));
        assert_eq!(
            server_wide(&raw, &module_level),
            Some(ServerWide::ModuleLevel)
        );
    }

    /// An archive under a directory of its own, removed first, holding a
    /// manifest of no points, the model and declaration every archive
    /// holds, and `extra`.
    fn archive(label: &str, extra: Vec<EvidenceArchiveEntry>) -> ArchiveReportRequest {
        let root =
            std::env::temp_dir().join(format!("supercov-strong-{}-{label}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let json = |path: &str, value: serde_json::Value| EvidenceArchiveEntry {
            path: path.into(),
            contents: serde_json::to_vec(&value).unwrap(),
        };
        let model = crate::coverage_report::PersistedCoverageModel::from_declaration(
            &crate::coverage_report::javascript_coverage_model(),
        )
        .unwrap();
        let mut entries = vec![
            json(
                "manifest.json",
                serde_json::json!({"decisions": [], "points": [], "branches": []}),
            ),
            json("coverage-model.json", serde_json::to_value(model).unwrap()),
            json(
                "frontend.json",
                serde_json::json!({
                    "protocolVersion": 2,
                    "frontendId": "javascript",
                    "frontendVersion": "fixture-v1",
                    "language": "javascript",
                    "structuralSource": "owned-probes",
                    "runners": [{
                        "runner": "vitest",
                        "executionModel": "serial-in-process",
                        "attribution": {
                            "run": "exact", "worker": "unavailable", "test": "exact",
                            "retry": "exact", "phase": "exact", "action": "exact",
                            "assertion": "exact"
                        },
                        "limitations": [{
                            "id": "fixture-worker-unavailable",
                            "scopes": ["worker"],
                            "reason": "The fixture does not require worker identity"
                        }]
                    }],
                    "structuralLimitations": []
                }),
            ),
            json(
                "worker/mcdc.json",
                serde_json::to_value(RawTestResult {
                    retry: Some(0),
                    ..record("a", "test", Some("t/a.test.js"), &[])
                })
                .unwrap(),
            ),
        ];
        entries.extend(extra);
        let archive_path = root.join("evidence.raw.gz");
        write_archive(entries, &archive_path).unwrap();
        ArchiveReportRequest {
            archive_path,
            run_id: "run".into(),
            generated_at: "time".into(),
            integrity: None,
            test_exit_code: ExitCodeInput::Missing,
            test_filter: None,
        }
    }

    fn own_mutants() -> EvidenceArchiveEntry {
        EvidenceArchiveEntry {
            path: "mutants.json".into(),
            contents: serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "mutantSet": crate::js_mutants::MUTANT_SET,
                "files": {}
            }))
            .unwrap(),
        }
    }

    #[test]
    fn a_run_whose_archive_does_not_read_has_no_plan_and_lists_nothing() {
        let request = archive("unread", Vec::new());
        let missing = ArchiveReportRequest {
            archive_path: request.archive_path.with_file_name("missing.raw.gz"),
            ..request
        };
        let root = missing.archive_path.parent().unwrap().to_owned();
        assert!(matches!(
            plan_for_archive(&missing, &root),
            Err(ReportError::InvalidArchive(_))
        ));
        assert!(matches!(
            list_run_mutants(&missing, &root),
            Err(ReportError::InvalidArchive(_))
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A run that measured no file and listed no mutants has nothing to
    /// list, and leaves nothing beside its archive.
    #[test]
    fn a_run_that_measured_no_file_has_no_plan_and_nothing_listed_beside_it() {
        let request = archive("no-points", Vec::new());
        let root = request.archive_path.parent().unwrap().to_owned();
        let planned = plan_for_archive(&request, &root).unwrap();
        assert!(planned.plan.is_none());
        assert!(planned.changed.is_empty());
        assert!(!beside(&request).exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Where the note beside the archive cannot be written, the plan and the
    /// listing fail, and say where.
    #[test]
    fn a_note_beside_the_archive_that_cannot_be_written_fails_the_plan_and_the_listing() {
        let request = archive("unwritable", vec![own_mutants()]);
        let root = request.archive_path.parent().unwrap().to_owned();
        std::fs::create_dir(beside(&request).with_extension("json.partial")).unwrap();
        for error in [
            plan_for_archive(&request, &root).err(),
            list_run_mutants(&request, &root).err(),
        ] {
            assert!(
                matches!(&error, Some(ReportError::InvalidArchive(message))
                    if message.contains(crate::coverage_report::LISTED_MUTANTS_FILE)),
                "{error:?}"
            );
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A note beside the archive that does not read says nothing: the run's
    /// own list does.
    #[test]
    fn a_note_beside_the_archive_that_does_not_read_leaves_the_run_s_own_list() {
        let request = archive("unreadable-note", vec![own_mutants()]);
        let root = request.archive_path.parent().unwrap().to_owned();
        std::fs::write(beside(&request), b"not json").unwrap();
        assert!(run_recorded_mutant_values(&request.archive_path));
        std::fs::remove_dir_all(root).unwrap();
    }
}
