//! Strong mutation's plan (`docs/design/strong-mutation.md`): which of a
//! run's mutants need running, and against which of its tests.
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
use crate::js_mutant_links::{CandidateMutant, MutantsFile};
use crate::js_weak_mutation::{Observations, Status, observations, verdict};

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
        && let Ok(listed) = serde_json::from_slice::<crate::js_mutant_links::MutantsFile>(&bytes)
    {
        // An empty list there marks a run that had its own.
        return listed.mutant_set.is_none();
    }
    let mut listed = false;
    let _ = crate::evidence_archive::read_archive_each(
        archive_path,
        |path| path == "mutants.json",
        |entry| {
            listed = serde_json::from_slice::<crate::js_mutant_links::MutantsFile>(&entry.contents)
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
    listed: Option<crate::js_mutant_links::MutantsFile>,
    points: &std::collections::BTreeMap<String, std::collections::BTreeSet<String>>,
) -> Result<(Option<crate::js_mutant_links::MutantsFile>, Vec<String>), ReportError> {
    let path = beside(request);
    let (kept, written, changed) = match listed {
        Some(own) if own.mutant_set.is_some() => {
            let none = crate::js_mutant_links::MutantsFile {
                schema_version: own.schema_version,
                mutant_set: None,
                files: std::collections::BTreeMap::new(),
                tests_without_values: Vec::new(),
            };
            (Some(own), none, Vec::new())
        }
        _ if points.is_empty() => return Ok((None, Vec::new())),
        _ => {
            let fresh = crate::js_mutant_links::list_mutants(root, points);
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
            if let Some(file) = &raw.test_file {
                by_file
                    .entry(file.clone())
                    .or_default()
                    .push(tests.len() - 1);
            }
            tests.len() - 1
        });
        runnable[index] &= passed(raw);
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
    for (test, ok) in tests.iter_mut().zip(&runnable) {
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
                        if role_of(raw) == "test"
                            && (may_reach(mutant, own)
                                || mutant
                                    .function
                                    .as_ref()
                                    .is_some_and(|function| own.hits.contains(function)))
                            && let Some(&test) = test_index
                                .get(&raw.test_id.clone().unwrap_or_else(|| raw.test.clone()))
                        {
                            covering.insert(test);
                        }
                        let why = if role_of(raw) == "test" && !exact(raw) {
                            Some(CandidateReason::NotExact)
                        } else if reach_only {
                            (may_reach(mutant, own)
                                || mutant
                                    .function
                                    .as_ref()
                                    .is_some_and(|function| own.hits.contains(function)))
                            .then_some(CandidateReason::Unknown)
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
                                if let Some(&test) = test_index.get(&id) {
                                    take(test, why);
                                }
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
