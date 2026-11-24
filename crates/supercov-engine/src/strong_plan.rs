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

fn is_zero(count: &usize) -> bool {
    *count == 0
}

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
    /// Its own work read the module-level value the mutant is in.
    Read,
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
    /// Set where a server process ran its code for one of its candidates.
    /// A switch the strong build places where the module loads (`static`
    /// in its list: an arrow mutated whole, defined at a module's top
    /// level) is then evaluated once, as the server loads the module, and
    /// what it made serves every later request whatever mutant the request
    /// names: such a mutant, too, runs with the server started with it.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub server_ran: bool,
    /// How many of its candidates passed with it in earlier passes, which
    /// this pass does not run again (`strong_verdicts::resume`): a mutant
    /// some candidate let live is unlikely to fail its next one.
    #[serde(skip_serializing_if = "is_zero")]
    pub passed_before: usize,
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
    /// Set for a module-level mutant whose value is read through its literal:
    /// how many candidates the module's loading gave it, and how many read it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrowed: Option<Narrowed>,
}

/// A module-level mutant's candidates before and after its value's reads
/// narrowed them (`docs/design/strong-mutation.md`, "Module-level values").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Narrowed {
    /// The candidates of every test that loaded its module.
    pub from: usize,
    /// The candidates of the tests that read its value.
    pub to: usize,
    /// A read while a module loaded carried the value elsewhere: its
    /// candidates stay those of every test that loaded the module.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub escaped: bool,
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
    /// Partial progress: for each mutant with no verdict yet (or an
    /// `error`), the ids of its candidate tests that already passed with it
    /// switched on. A later pass runs it only against the rest of its
    /// candidates (`docs/design/strong-mutation.md`, "Partial progress").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub progress: Progress,
}

/// A mutant's candidate tests that passed with it, by mutant id: what
/// [`StrongVerdicts::progress`] holds.
pub type Progress = BTreeMap<String, std::collections::BTreeSet<String>>;

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

/// The plan for a stored run, as `plan_for_archive` makes it of its archive.
pub fn plan_for_run(
    run: &crate::run_store::StoredRun,
    root: &std::path::Path,
) -> Result<ArchivePlan, ReportError> {
    plan_for_archive(
        &ArchiveReportRequest {
            archive_path: run.evidence_path.clone(),
            run_id: run.evidence_run_id().to_owned(),
            generated_at: run.metadata.started_at.clone(),
            integrity: Some(serde_json::to_value(&run.metadata.integrity).expect("plain data")),
            test_exit_code: crate::coverage_report::ExitCodeInput::Present(
                run.metadata.test_exit_code,
            ),
            test_filter: None,
        },
        root,
    )
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

/// A Python test the measured run skipped: it still runs in a strong pass,
/// as mutmut runs it, since a mutant can turn a skip into a failure (a
/// subtest skipped before an assertion that fails). Its baseline in the pass
/// says whether it passes without the mutant.
fn skipped_python(raw: &RawTestResult) -> bool {
    raw.status.as_deref() == Some("skipped")
        && !raw.flaky
        && crate::python_strong::RUNNERS.contains(&raw.provenance.runner.as_str())
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
        // Looked up in a set: a decision a test file's modules and its test
        // both took holds many vectors, and every test of the file is merged
        // with them.
        let mut known = seen.iter().cloned().collect::<HashSet<_>>();
        for vector in vectors {
            if known.insert(vector.clone()) {
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
    may_reach_also_hit(mutant, seen, None)
}

/// `may_reach`, with `also_hit` counted among what `seen` hit.
fn may_reach_also_hit(
    mutant: &CandidateMutant,
    seen: &Observations,
    also_hit: Option<&str>,
) -> bool {
    let hit = |point: &String| also_hit == Some(point.as_str()) || seen.hits.contains(point);
    mutant.statement.as_ref().is_none_or(hit) && mutant.function.as_ref().is_none_or(hit)
}

/// Whether `seen` could have infected the mutant: its verdict read from
/// `seen` alone is `infected` or `unknown`.
fn could_infect(
    mutant: &CandidateMutant,
    seen: &Observations,
    whole: &Observations,
    language: &str,
) -> Option<CandidateReason> {
    // A method's body is reached when its function runs, though the
    // statement around the function (the export of the object it is on)
    // ran once, when the module loaded, in whichever test loaded it first.
    // Where this test ran the function and the run ran the statement, the
    // statement counts as run here: it only ever adds a candidate. Only a
    // JavaScript mutant's statement can hold its function: a Python
    // mutant's is always inside it (`python_mutants`). It is counted, not
    // added to a copy of what the test saw: copying a record's observations
    // for each mutant it ran was most of planning a large run.
    let widened = match (&mutant.statement, &mutant.function) {
        (Some(statement), Some(function))
            if language == crate::mutant_list::JAVASCRIPT
                && seen.hits.contains(function)
                && !seen.hits.contains(statement)
                && whole.hits.contains(statement) =>
        {
            Some(statement.as_str())
        }
        _ => None,
    };
    if !may_reach_also_hit(mutant, seen, widened) {
        return None;
    }
    match crate::js_weak_mutation::verdict_also_hit(mutant, seen, widened).0 {
        Status::Infected { .. } => Some(CandidateReason::Infected),
        Status::Unknown => Some(CandidateReason::Unknown),
        Status::NotReached | Status::NotInfected | Status::Equivalent => None,
    }
}

/// The points a record's server processes ran, each with whether a request
/// named none of those runs, whether one did, and whether one was a request
/// no test was credited with, sent while tests ran
/// (`coverage_report::REQUEST_DURING_TESTS`): read once a record, not once a
/// mutant, as a record of a large suite holds thousands.
type ServerRuns<'r> = HashMap<&'r str, (bool, bool, bool)>;

fn server_runs(raw: &RawTestResult) -> ServerRuns<'_> {
    let mut runs = ServerRuns::new();
    for record in raw
        .server
        .iter()
        .filter(|record| record.record_type == "hit")
    {
        if let Some(id) = &record.id {
            let ran = runs.entry(id.as_str()).or_default();
            ran.0 |= record.request.is_none();
            ran.1 |= record.request.is_some();
            ran.2 |= record.request == Some(crate::coverage_report::REQUEST_DURING_TESTS);
        }
    }
    runs
}

/// For a record no test was credited with: each point its servers ran, with
/// the moments requests that named no test ran it, or `None` where some of
/// that work was no request's, kept no time, or the record ran the point
/// outside its servers too (a test runner's own process, a page). Built only
/// for such records; a test's own record needs none.
type RequestTimes<'r> = HashMap<&'r str, Option<Vec<i64>>>;

fn request_times(raw: &RawTestResult) -> RequestTimes<'_> {
    let mut times = RequestTimes::new();
    for record in raw
        .server
        .iter()
        .filter(|record| record.record_type == "hit")
    {
        if let Some(id) = &record.id {
            let at = record.request.and(record.timestamp_ms);
            let seen = times.entry(id.as_str()).or_insert_with(|| Some(Vec::new()));
            match (seen.as_mut(), at) {
                (Some(moments), Some(at)) => moments.push(at),
                _ => *seen = None,
            }
        }
    }
    for snapshot in raw.runtime.iter().chain(&raw.browser) {
        let events = snapshot
            .events
            .iter()
            .filter(|event| event.event_type == "hit")
            .map(|event| &event.id);
        for id in snapshot.hits.iter().chain(events) {
            if let Some(seen) = times.get_mut(id.as_str()) {
                *seen = None;
            }
        }
    }
    for moments in times.values_mut().flatten() {
        moments.sort_unstable();
        moments.dedup();
    }
    times
}

/// When each test ran, as `(start, end, test)`: each of its records' span
/// (its attempt's first and last moment), or its phases where it has none.
/// The time the measured run credits a request's work by
/// (`coverage_report`'s `credit_by_time`), taken as widely.
fn test_windows(
    raw_results: &[RawTestResult],
    test_index: &HashMap<String, usize>,
) -> Vec<(i64, i64, usize)> {
    let mut windows = raw_results
        .iter()
        .filter(|raw| role_of(raw) == "test")
        .filter_map(|raw| {
            let test = *test_index.get(raw.test_id.as_ref().unwrap_or(&raw.test))?;
            let phases = || {
                let start = raw.phases.iter().map(|phase| phase.started_at_ms).min()?;
                let end = raw
                    .phases
                    .iter()
                    .map(|phase| phase.ended_at_ms.unwrap_or(phase.started_at_ms))
                    .max()?;
                Some([start, end])
            };
            let [start, end] = raw.span.or_else(phases)?;
            Some((start, end, test))
        })
        .collect::<Vec<_>>();
    windows.sort_unstable();
    windows
}

/// The tests running at each of `moments` (sorted), or `None` where one of
/// them fell while no test ran: work no test was running for may be any
/// test's to see, and the server's whole start too.
fn running_at(windows: &[(i64, i64, usize)], moments: &[i64]) -> Option<Vec<usize>> {
    let mut tests = Vec::new();
    for &at in moments {
        // Windows are sorted by start: those after `at` cannot hold it.
        let started = windows.partition_point(|(start, _, _)| *start <= at);
        let before = tests.len();
        tests.extend(
            windows[..started]
                .iter()
                .filter(|(_, end, _)| *end >= at)
                .map(|(_, _, test)| *test),
        );
        if tests.len() == before {
            return None;
        }
    }
    tests.sort_unstable();
    tests.dedup();
    Some(tests)
}

/// Whether a server process ran the mutant's code in this record (`runs`,
/// its `server_runs`) in a way a request's own mutant cannot reach, and how.
fn server_wide(
    raw: &RawTestResult,
    runs: &ServerRuns<'_>,
    mutant: &CandidateMutant,
) -> Option<ServerWide> {
    let ran = [&mutant.statement, &mutant.function]
        .into_iter()
        .flatten()
        .filter_map(|point| runs.get(point.as_str()))
        .collect::<Vec<_>>();
    if ran.is_empty() {
        return None;
    }
    let at_load = mutant.function.is_none();
    if !matches!(role_of(raw), "test" | "setup") {
        // Work for a request that named no test and ran while none did (a
        // web server's readiness check) ran without any mutant in the pass,
        // as each request runs with its own; only work no request started
        // (start-up, timers) needs the server started with the mutant.
        if ran.iter().any(|(unrequested, _, _)| *unrequested) {
            return Some(ServerWide::Background);
        }
        if at_load {
            return Some(ServerWide::ModuleLevel);
        }
        // A request that named no test, sent while tests ran (several at
        // once, so it is none of theirs by time): one of them may have sent
        // it without its name (`curl` from the test, a client the runtime
        // does not reach), and no mutant a request names reaches it there.
        return ran
            .iter()
            .any(|(_, _, during_tests)| *during_tests)
            .then_some(ServerWide::ByTime);
    }
    if at_load {
        return Some(ServerWide::ModuleLevel);
    }
    ran.iter()
        .any(|(_, requested, _)| *requested)
        .then_some(ServerWide::ByTime)
}

/// Whether a server process ran the mutant's code in this record (`runs`,
/// its `server_runs`).
fn server_ran(runs: &ServerRuns<'_>, mutant: &CandidateMutant) -> bool {
    [&mutant.statement, &mutant.function]
        .into_iter()
        .flatten()
        .any(|point| runs.contains_key(point.as_str()))
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
                // Vitest's Browser Mode runs a test in the browser, where
                // a mutant is switched on another way than in its workers.
                runner: if raw
                    .scope
                    .as_ref()
                    .is_some_and(|scope| scope.worker_id == "vitest-browser")
                {
                    "vitest-browser".to_owned()
                } else {
                    raw.provenance.runner.clone()
                },
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
            runnable[index] &= passed(raw) || skipped_python(raw);
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
    for ((test, ok), stated) in tests.iter_mut().zip(&mut runnable).zip(&stated) {
        *ok &= *stated;
        test.runnable = *ok;
    }
    let windows = test_windows(raw_results, &test_index);
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
    // Each record's on its own, on every core: a merged run of shards that
    // each measured the whole suite holds thousands of records.
    let observed = crate::threads::parallel_map(raw_results, |raw| {
        let mut own = own_observations(raw, &without_values);
        if role_of(raw) == "test"
            && let Some(at_load) = raw.test_file.as_deref().and_then(|file| loaded.get(file))
        {
            merge(&mut own, at_load);
        }
        own
    });
    for (record, (raw, own)) in raw_results.iter().zip(observed).enumerate() {
        let at = seen.len();
        if role_of(raw) == "test" && !exact(raw) {
            unbounded.push(at);
        }
        for point in &own.hits {
            by_point.entry(point.clone()).or_default().push(at);
        }
        seen.push(Seen {
            record,
            seen: own,
            server: server_runs(raw),
            requests: (!matches!(role_of(raw), "test" | "setup")).then(|| request_times(raw)),
        });
    }

    // Each mutant is planned on its own, from what is read above, on every
    // core.
    let each = listed
        .files
        .iter()
        .flat_map(|(file, listed_mutants)| {
            listed_mutants
                .iter()
                .enumerate()
                .map(move |(index, mutant)| (file, index, mutant))
        })
        .collect::<Vec<_>>();
    let mutants = crate::threads::parallel_map(&each, |&(file, index, mutant)| {
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
            server_ran: false,
            passed_before: 0,
            covering: 0,
            statement: mutant.statement.clone(),
            function: mutant.function.clone(),
            function_hit_by: BTreeMap::new(),
            narrowed: None,
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
                let context = Selecting {
                    raw_results,
                    seen: &seen,
                    by_point: &by_point,
                    unbounded: &unbounded,
                    tests: &tests,
                    test_index: &test_index,
                    by_file: &by_file,
                    whole: &whole,
                    language: &planned.language,
                    windows: &windows,
                };
                let broad = context.select(mutant, reach_only, None);
                // A module-level value read through its literal: the
                // tests that read it, unless a read while a module loaded
                // carried it elsewhere.
                let reads = mutant
                    .reads
                    .as_ref()
                    .filter(|_| mutant.function.is_none() && !reach_only);
                let mut chosen = broad.chosen.clone();
                let mut whole_files = broad.whole_files.clone();
                if let Some(reads) = reads {
                    let broad_count = broad.chosen.keys().filter(|test| runnable[**test]).count();
                    if whole.hits.contains(&reads.escape) {
                        planned.narrowed = Some(Narrowed {
                            from: broad_count,
                            to: broad_count,
                            escaped: true,
                        });
                    } else {
                        let narrow = context.select(mutant, reach_only, Some(reads));
                        planned.narrowed = Some(Narrowed {
                            from: broad_count,
                            to: narrow.chosen.keys().filter(|test| runnable[**test]).count(),
                            escaped: false,
                        });
                        chosen = narrow.chosen;
                        whole_files = narrow.whole_files;
                    }
                }
                planned.server_wide = broad.server_wide;
                planned.server_ran = broad.server_ran;
                // No test ran its code: what ran it was work no test of
                // the run could have caused (a run with no test, a test
                // file's modules loaded with no test of it), and no test
                // can kill it. StrykerJS calls it NoCoverage.
                let tested = !broad.chosen.is_empty();
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
                if !tested {
                    planned.decided = Some(Decided::NoCoverage);
                    planned.reason = Some("no-test-ran-it".into());
                } else if candidates.is_empty() {
                    planned.decided = Some(Decided::Survived);
                    planned.reason = Some(
                        // Narrowed to none of the tests that could run.
                        if planned.narrowed.is_some()
                            && broad.chosen.keys().any(|test| runnable[*test])
                        {
                            "no-test-read-it"
                        } else {
                            "no-passing-candidate"
                        }
                        .into(),
                    );
                }
                let mut whole_files = whole_files.into_iter().collect::<Vec<_>>();
                whole_files.sort();
                planned.candidates = candidates;
                planned.whole_files = whole_files;
                planned.covering = broad.covering;
            }
        }
        planned
    });
    Some(StrongPlan {
        mutant_set,
        tests,
        mutants,
    })
}

/// One record's own observations, by its place among the run's.
struct Seen<'r> {
    record: usize,
    seen: Observations,
    server: ServerRuns<'r>,
    /// For a record no test was credited with, when requests that named no
    /// test ran each point (`request_times`).
    requests: Option<RequestTimes<'r>>,
}

/// A mutant's candidates, the test files it runs whole, and how a server
/// must run it.
struct Selection {
    chosen: BTreeMap<usize, CandidateReason>,
    whole_files: HashSet<String>,
    server_wide: Option<ServerWide>,
    /// A server ran its code for a record it may be killed by.
    server_ran: bool,
    /// How many tests ran its code.
    covering: usize,
}

/// What choosing a mutant's candidates reads.
struct Selecting<'r> {
    raw_results: &'r [RawTestResult],
    seen: &'r [Seen<'r>],
    by_point: &'r HashMap<String, Vec<usize>>,
    unbounded: &'r [usize],
    tests: &'r [PlannedTest],
    test_index: &'r HashMap<String, usize>,
    by_file: &'r BTreeMap<String, Vec<usize>>,
    whole: &'r Observations,
    language: &'r str,
    /// When each test ran (`test_windows`).
    windows: &'r [(i64, i64, usize)],
}

impl Selecting<'_> {
    /// The tests that may have sent the requests in which work no test was
    /// credited with ran the mutant's code: those running when each request
    /// ran it. `None` (every test) where some of that work was no request's
    /// (a server's start, a timer), kept no time, came while no test ran,
    /// or ran outside the servers, and for a mutant with no point to look
    /// for.
    ///
    /// A test that was not running when the code ran neither sent the
    /// request nor saw its response, and its own evidence never ran the
    /// code: it runs as without the mutant, as the plan takes of every test
    /// whose own evidence did not reach a mutant. What the request left in
    /// shared state for a later test is that test's to observe without a
    /// read, as a by-time credit to the one test running already assumes.
    fn during_requests(
        &self,
        mutant: &CandidateMutant,
        own: &Observations,
        requests: &RequestTimes<'_>,
    ) -> Option<Vec<usize>> {
        let mut moments = Vec::new();
        let mut looked = false;
        for point in [&mutant.statement, &mutant.function].into_iter().flatten() {
            if !own.hits.contains(point) {
                continue;
            }
            looked = true;
            moments.extend(requests.get(point.as_str())?.as_ref()?.iter().copied());
        }
        if !looked {
            return None;
        }
        moments.sort_unstable();
        moments.dedup();
        running_at(self.windows, &moments)
    }

    /// The mutant's candidates. With `reads`, those of a module-level value
    /// read through its literal (`docs/design/strong-mutation.md`,
    /// "Module-level values"): its module's loading is not a read, so a
    /// record that only loaded it takes no test, but where the loading was
    /// in a test runner's own process, whose later tests share the module;
    /// and a record that read it takes the tests as one that infected it
    /// does.
    fn select(
        &self,
        mutant: &CandidateMutant,
        reach_only: bool,
        reads: Option<&crate::mutant_list::ReadLink>,
    ) -> Selection {
        let mut chosen = BTreeMap::<usize, CandidateReason>::new();
        let mut whole_files = HashSet::<String>::new();
        let mut server_wide = None;
        let mut server_ran = false;
        let mut take = |test: usize, why: CandidateReason| {
            chosen.entry(test).or_insert(why);
        };
        // The records that ran the mutant's point (or all, for a mutant no
        // point contains), those that read it, and every record whose
        // silence proves nothing. Reach needs both points; the function's is
        // the narrower.
        let point = mutant.function.as_ref().or(mutant.statement.as_ref());
        let mut records = match point {
            Some(point) => self.by_point.get(point).cloned().unwrap_or_default(),
            None => (0..self.seen.len()).collect(),
        };
        if let Some(reads) = reads {
            records.extend(self.by_point.get(&reads.read).into_iter().flatten());
        }
        records.extend(self.unbounded.iter().copied());
        records.sort_unstable();
        records.dedup();
        let mut covering = HashSet::<usize>::new();
        for at in records {
            let Seen {
                record,
                seen: own,
                server,
                requests,
            } = &self.seen[at];
            let raw = &self.raw_results[*record];
            let role = role_of(raw);
            let test = (role == "test")
                .then(|| self.test_index[&raw.test_id.clone().unwrap_or_else(|| raw.test.clone())]);
            // Every test record was indexed by this id above.
            if let Some(test) = test
                && (may_reach(mutant, own)
                    || mutant
                        .function
                        .as_ref()
                        .is_some_and(|function| own.hits.contains(function)))
            {
                covering.insert(test);
            }
            let why = if test.is_some() && !exact(raw) {
                Some(CandidateReason::NotExact)
            } else if reach_only {
                // Every record read here ran the mutant's point, or the
                // mutant has none, but an inexact test's, which was taken
                // above.
                Some(CandidateReason::Unknown)
            } else {
                could_infect(mutant, own, self.whole, self.language)
            };
            if why.is_some() && server_wide.is_none() {
                server_wide = self::server_wide(raw, server, mutant);
            }
            if why.is_some() && !server_ran {
                server_ran = self::server_ran(server, mutant);
            }
            let why = match reads {
                None => why,
                Some(reads) if own.hits.contains(&reads.read) => Some(CandidateReason::Read),
                Some(_) => match (why, test) {
                    (Some(CandidateReason::NotExact), _) => Some(CandidateReason::NotExact),
                    // Loaded in a test runner's own process: its later tests
                    // share the module, as without reads.
                    (Some(why), Some(test)) if self.tests[test].runner != "playwright" => Some(why),
                    (Some(why), None) if role == "setup" && raw.test_file.is_some() => Some(why),
                    _ => None,
                },
            };
            let Some(why) = why else { continue };
            match (role, &raw.test_file) {
                ("test", _) => {
                    let test = test.expect("a test record names its test");
                    take(test, why);
                    // Module-level code a test ran (a module it loaded
                    // first): the module keeps what it made for every later
                    // test of the file, which runs none of that code again.
                    // A browser test's modules are its server's and its
                    // page's, not its file's.
                    if mutant.function.is_none()
                        && (reads.is_none() || self.tests[test].runner != "playwright")
                        && let Some(file) = &raw.test_file
                    {
                        whole_files.insert(file.clone());
                        for &test in self.by_file.get(file).into_iter().flatten() {
                            take(test, CandidateReason::ModuleLoad);
                        }
                    }
                }
                // A file's module setup: every test of the file.
                ("setup", Some(file)) => {
                    whole_files.insert(file.clone());
                    for &test in self.by_file.get(file).into_iter().flatten() {
                        take(test, CandidateReason::ModuleLoad);
                    }
                }
                // Work no test was credited with, a module setup that names
                // no test file among it (Mocha loads every test file before
                // the first test): any test of the run may have caused it.
                // Where it was only requests that named no test, each sent
                // while tests ran, the tests running then are those that may
                // have (`during_requests`).
                _ => {
                    let during = requests
                        .as_ref()
                        .and_then(|requests| self.during_requests(mutant, own, requests));
                    match during {
                        Some(tests) => {
                            for test in tests {
                                take(test, CandidateReason::Unattributed);
                            }
                        }
                        None => {
                            for test in 0..self.tests.len() {
                                take(test, CandidateReason::Unattributed);
                            }
                        }
                    }
                }
            }
        }
        Selection {
            chosen,
            whole_files,
            server_wide,
            server_ran,
            covering: covering.len(),
        }
    }
}

/// What a copy of a test file costs beside its tests' own time, in
/// milliseconds: a worker starting and the file loading. The Vitest driver
/// guesses the same before it has timed a copy.
pub const COPY_START_MS: u64 = 300;

/// A test the measured run gave no duration.
pub const UNTIMED_TEST_MS: u64 = 1_000;

impl StrongPlan {
    /// The tests of each test file, by index, in the plan's order.
    fn tests_by_file(&self) -> HashMap<&str, Vec<usize>> {
        let mut by_file = HashMap::<&str, Vec<usize>>::new();
        for (index, test) in self.tests.iter().enumerate() {
            if let Some(file) = &test.file {
                by_file.entry(file.as_str()).or_default().push(index);
            }
        }
        by_file
    }

    /// The copies a mutant runs if none kills it: for each test file, the
    /// tests it runs there (its candidates, or every test of a file it runs
    /// whole). Candidates with no file make one copy of their own.
    fn copies_of<'a>(
        &'a self,
        mutant: &'a PlannedMutant,
        by_file: &HashMap<&str, Vec<usize>>,
    ) -> BTreeMap<Option<&'a str>, Vec<usize>> {
        let mut copies = BTreeMap::<Option<&str>, Vec<usize>>::new();
        for candidate in &mutant.candidates {
            let file = self
                .tests
                .get(candidate.test)
                .and_then(|test| test.file.as_deref());
            copies.entry(file).or_default().push(candidate.test);
        }
        for file in &mutant.whole_files {
            copies.insert(
                Some(file.as_str()),
                by_file.get(file.as_str()).cloned().unwrap_or_default(),
            );
        }
        copies
    }

    /// A mutant's cost if it survives, in milliseconds: each copy's start
    /// and its tests' measured durations. A kill ends its mutant's copies
    /// early, so a killed mutant costs less than this.
    fn estimated_ms_with(
        &self,
        mutant: &PlannedMutant,
        by_file: &HashMap<&str, Vec<usize>>,
    ) -> u64 {
        self.copies_of(mutant, by_file)
            .values()
            .map(|tests| {
                COPY_START_MS
                    + tests
                        .iter()
                        .map(|test| {
                            self.tests
                                .get(*test)
                                .and_then(|test| test.duration_ms)
                                .unwrap_or(UNTIMED_TEST_MS)
                        })
                        .sum::<u64>()
            })
            .sum()
    }

    /// Each mutant's estimated cost (`estimated_ms_with`), by id: what
    /// orders a pass and says how far it got.
    pub fn estimated_ms(&self) -> BTreeMap<String, u64> {
        let by_file = self.tests_by_file();
        self.mutants
            .iter()
            .map(|mutant| (mutant.id.clone(), self.estimated_ms_with(mutant, &by_file)))
            .collect()
    }

    /// Orders the mutants cheapest first, by their estimated cost, the
    /// plan's order between equals: a pass cut short (a budget, a job's time
    /// limit) has then decided as many as it could.
    pub fn cheapest_first(&mut self) {
        let costs = self.estimated_ms();
        self.mutants
            .sort_by_key(|mutant| costs.get(&mutant.id).copied().unwrap_or(0));
    }

    /// Keeps the k-th of n shares of the mutants to run, the decided ones in
    /// the first. The shares have about the same cost: the mutants are dealt
    /// in turn, back and forth, most costly first, so every share gets its
    /// part of the costly ones. The cost read here is what the plan lists --
    /// how many copies a mutant starts and how many tests they run -- not
    /// the tests' times: each job of a sharded pass measures the suite
    /// itself, the times differ between jobs, and a split that read them
    /// would put some mutants in no share and some in two.
    pub fn keep_share(&mut self, k: usize, n: usize) {
        let by_file = self.tests_by_file();
        let mut undecided = self
            .mutants
            .iter()
            .enumerate()
            .filter(|(_, mutant)| mutant.decided.is_none())
            .map(|(position, mutant)| {
                let weight = self
                    .copies_of(mutant, &by_file)
                    .values()
                    .map(|tests| 1 + tests.len())
                    .sum::<usize>();
                (std::cmp::Reverse(weight), position)
            })
            .collect::<Vec<_>>();
        undecided.sort_unstable();
        let mut shares = vec![0usize; self.mutants.len()];
        for (dealt, (_, position)) in undecided.into_iter().enumerate() {
            let (round, seat) = (dealt / n, dealt % n);
            shares[position] = if round % 2 == 0 { seat } else { n - 1 - seat };
        }
        let mut position = 0;
        self.mutants.retain(|mutant| {
            let share = shares[position];
            position += 1;
            if mutant.decided.is_some() {
                k == 1
            } else {
                share == k - 1
            }
        });
    }
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

    /// Work no test was credited with, done for requests that named no test
    /// while tests ran (a webhook a mock server sends while several tests
    /// run): its mutants' candidates are the tests running at those moments,
    /// not every test of the run. Where any of that work came while no test
    /// ran, was a server's start (no request), kept no time, or also ran
    /// outside the servers, every test stays a candidate.
    #[test]
    fn work_for_requests_no_test_named_takes_the_tests_running_then() {
        let browser = |id: &str, span: [i64; 2]| {
            let mut raw = record(id, "test", Some("t/a.spec.js"), &[]);
            raw.provenance.runner = "playwright".into();
            raw.span = Some(span);
            raw
        };
        let at = |id: &str, request: Option<u64>, ms: Option<i64>| ServerRecord {
            timestamp_ms: ms,
            ..server_hit(id, request)
        };
        let mut background = record("bg", "background", None, &["runner"]);
        background.server = vec![
            // Two requests, at 150 (a and b ran) and 450 (d ran).
            at("hook", Some(0), Some(150)),
            at("hook", Some(0), Some(450)),
            // One while no test ran.
            at("between", Some(0), Some(150)),
            at("between", Some(0), Some(250)),
            // The server's start: no request.
            at("start", None, Some(1)),
            at("start", Some(0), Some(150)),
            // A request that kept no time.
            at("untimed", Some(0), None),
            // A request, and the test runner's own process too.
            at("runner", Some(0), Some(150)),
        ];
        let raw_results = vec![
            background,
            browser("a", [100, 200]),
            browser("b", [120, 220]),
            browser("c", [300, 400]),
            browser("d", [400, 500]),
        ];
        let mutants = ["hook", "between", "start", "untimed", "runner"]
            .into_iter()
            .map(|point| mutant(serde_json::json!({"id": point, "statement": point})))
            .collect::<Vec<_>>();
        let plan = plan(&raw_results, listed(mutants, &[])).unwrap();
        let names = |id: &str| {
            candidates(&plan, id)
                .into_iter()
                .map(|(name, reason)| {
                    assert_eq!(reason, CandidateReason::Unattributed);
                    name
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(names("hook"), ["a", "b", "d"]);
        let every = ["a", "b", "c", "d"];
        for id in ["between", "start", "untimed", "runner"] {
            assert_eq!(names(id), every, "{id}");
        }
    }

    /// A module-level mutant whose value is read through its literal, and
    /// the records of a browser suite against one server: the server loaded
    /// the module as it started, one test's request loaded nothing new but
    /// read the value, one loaded another route's modules, and one did
    /// neither.
    fn read_through_literal() -> (CandidateMutant, Vec<RawTestResult>) {
        let module_level = mutant(serde_json::json!({
            "id": "m", "statement": "s", "reads": {"read": "r", "escape": "e"}
        }));
        let browser = |id: &str, file: &str, hits: &[&str]| {
            let mut raw = record(id, "test", Some(file), hits);
            raw.provenance.runner = "playwright".into();
            raw
        };
        let mut started = record("bg", "background", None, &[]);
        // A hit that names no point says nothing of any mutant.
        let unnamed = ServerRecord {
            id: None,
            ..server_hit("s", None)
        };
        started.server = vec![server_hit("s", None), unnamed];
        let raw_results = vec![
            started,
            browser("reader", "t/a.spec.js", &["r"]),
            browser("loader", "t/a.spec.js", &["s"]),
            browser("sibling", "t/a.spec.js", &[]),
            browser("other", "t/b.spec.js", &[]),
        ];
        (module_level, raw_results)
    }

    /// Only the tests that read a module-level value are its mutant's
    /// candidates: loading the module, at the server's start or in a test's
    /// request, is not a read, and a browser test's file shares nothing
    /// with the server. The plan says how far it narrowed them.
    #[test]
    fn a_module_level_value_s_candidates_are_the_tests_that_read_it() {
        let (module_level, raw_results) = read_through_literal();
        let plan = super::plan(&raw_results, listed(vec![module_level.clone()], &[])).unwrap();
        assert_eq!(
            candidates(&plan, "m"),
            [("reader".to_owned(), CandidateReason::Read)]
        );
        let narrowed = planned(&plan, "m");
        assert_eq!(
            narrowed.narrowed,
            Some(Narrowed {
                from: 4,
                to: 1,
                escaped: false
            })
        );
        assert!(narrowed.whole_files.is_empty());
        // The server still starts with it switched on, as it ran there.
        assert_eq!(narrowed.server_wide, Some(ServerWide::Background));
        assert_eq!(narrowed.covering, 1);
        let json = serde_json::to_value(narrowed).unwrap();
        assert_eq!(json["narrowed"], serde_json::json!({"from": 4, "to": 1}));
        assert_eq!(json["candidates"][0]["reason"], "read");

        // Read while a module loaded: what read it keeps it, and every test
        // that loaded the module stays a candidate.
        let mut escaped = raw_results.clone();
        escaped.push(record("bg2", "background", None, &["e"]));
        let plan = super::plan(&escaped, listed(vec![module_level.clone()], &[])).unwrap();
        assert_eq!(candidates(&plan, "m").len(), 4);
        assert_eq!(
            planned(&plan, "m").narrowed,
            Some(Narrowed {
                from: 4,
                to: 4,
                escaped: true
            })
        );

        // Read by work no test was credited with: any test may have caused
        // it. Read by a file's module setup: the file's tests.
        let mut background = raw_results.clone();
        background.push(record("bg3", "background", None, &["r"]));
        let plan = super::plan(&background, listed(vec![module_level.clone()], &[])).unwrap();
        assert_eq!(candidates(&plan, "m").len(), 4);
        let mut setup = raw_results.clone();
        setup.push(record("setup", "setup", Some("t/b.spec.js"), &["r"]));
        let plan = super::plan(&setup, listed(vec![module_level.clone()], &[])).unwrap();
        // (A test's evidence is read with its file's module setup's.)
        assert_eq!(
            candidates(&plan, "m"),
            [
                ("other".to_owned(), CandidateReason::Read),
                ("reader".to_owned(), CandidateReason::Read),
            ]
        );
        assert_eq!(planned(&plan, "m").whole_files, ["t/b.spec.js"]);

        // A test whose evidence is not its own stays a candidate.
        let mut inexact = raw_results.clone();
        inexact[4].attribution = ATTRIBUTION_RUN_WIDE.into();
        let plan = super::plan(&inexact, listed(vec![module_level.clone()], &[])).unwrap();
        assert_eq!(
            candidates(&plan, "m"),
            [
                ("other".to_owned(), CandidateReason::NotExact),
                ("reader".to_owned(), CandidateReason::Read),
            ]
        );

        // Read by no test: none can tell it apart.
        let unread = raw_results
            .iter()
            .filter(|raw| raw.test != "reader")
            .cloned()
            .collect::<Vec<_>>();
        let plan = super::plan(&unread, listed(vec![module_level.clone()], &[])).unwrap();
        let decided = planned(&plan, "m");
        assert!(decided.candidates.is_empty());
        assert_eq!(decided.decided, Some(Decided::Survived));
        assert_eq!(decided.reason.as_deref(), Some("no-test-read-it"));
        // Unless no test it could take would run: then as before.
        let mut failing = unread.clone();
        for raw in &mut failing {
            raw.status = Some("failed".into());
        }
        let plan = super::plan(&failing, listed(vec![module_level], &[])).unwrap();
        assert_eq!(
            planned(&plan, "m").reason.as_deref(),
            Some("no-passing-candidate")
        );
    }

    /// In a test runner's own process the module a test loaded serves the
    /// file's later tests, which read it without loading it: the loading
    /// test and its file stay candidates, as do a reading test's. A mutant
    /// in a function, or one only reach tells of, is not narrowed.
    #[test]
    fn a_module_loaded_in_the_runner_s_process_keeps_its_file() {
        let (module_level, raw_results) = read_through_literal();
        let in_process = raw_results
            .into_iter()
            .map(|mut raw| {
                raw.provenance.runner = "vitest".into();
                raw
            })
            .collect::<Vec<_>>();
        let plan = super::plan(&in_process, listed(vec![module_level.clone()], &[])).unwrap();
        assert_eq!(
            candidates(&plan, "m"),
            [
                ("loader".to_owned(), CandidateReason::ModuleLoad),
                ("reader".to_owned(), CandidateReason::Read),
                ("sibling".to_owned(), CandidateReason::ModuleLoad),
            ]
        );
        assert_eq!(planned(&plan, "m").whole_files, ["t/a.spec.js"]);
        // A file's module setup that loaded the module, in the runner's
        // process: the file's tests, as without reads.
        let mut setup = in_process.clone();
        setup.push(record("setup", "setup", Some("t/b.spec.js"), &["s"]));
        let plan = super::plan(&setup, listed(vec![module_level.clone()], &[])).unwrap();
        assert!(
            planned(&plan, "m")
                .whole_files
                .contains(&"t/b.spec.js".to_owned())
        );
        assert!(
            candidates(&plan, "m")
                .iter()
                .any(|(test, _)| test == "other")
        );
        let in_function = mutant(serde_json::json!({
            "id": "f", "statement": "s", "function": "fn",
            "reads": {"read": "r", "escape": "e"}
        }));
        let raw_results = [
            record("a", "test", Some("t/a.test.js"), &["s", "fn"]),
            record("b", "test", Some("t/a.test.js"), &["r"]),
        ];
        let plan = super::plan(&raw_results, listed(vec![in_function], &[])).unwrap();
        assert_eq!(planned(&plan, "f").narrowed, None);
        assert_eq!(
            candidates(&plan, "f"),
            [("a".to_owned(), CandidateReason::Infected)]
        );
        // A clock's value: no infection to read, so as before.
        let clock = mutant(serde_json::json!({
            "id": "c", "mutator": "ConditionalExpression", "replacement": "true",
            "original": "a > b", "statement": "s", "decision": {"id": "d"},
            "reads": {"read": "r", "escape": "e"},
            "untestable": {"rule": "clock", "reason": "reads the clock"}
        }));
        let mut loaded = record("a", "test", Some("t/a.test.js"), &["s"]);
        loaded.runtime[0].decisions = vec![decision("d", &[(true, true)])];
        let plan = super::plan(
            &[loaded, record("b", "test", Some("t/b.test.js"), &["r"])],
            listed(vec![clock], &[]),
        )
        .unwrap();
        let clocked = planned(&plan, "c");
        assert_eq!(clocked.weak, "not-infected");
        assert_eq!(clocked.narrowed, None);
        // Mocha's setup, which names no test file, loading the module takes
        // no test; a test with no file reading it takes itself alone.
        let mut setup = record("setup", "setup", None, &["s"]);
        setup.provenance.runner = "mocha".into();
        let mut reader = record("reader", "test", None, &["r"]);
        reader.provenance.runner = "mocha".into();
        let mut other = record("other", "test", Some("t/o.test.js"), &[]);
        other.provenance.runner = "mocha".into();
        let plan = super::plan(&[setup, reader, other], listed(vec![module_level], &[])).unwrap();
        assert_eq!(
            candidates(&plan, "m"),
            [("reader".to_owned(), CandidateReason::Read)]
        );
        assert!(planned(&plan, "m").whole_files.is_empty());
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
        // A record of evidence alone says nothing of the outcome, but for a
        // flaky attempt's.
        let mut evidence = record("d", "test", None, &[]);
        evidence.status = Some("unknown".into());
        let mut flaky_evidence = record("e", "test", None, &[]);
        flaky_evidence.status = Some("unknown".into());
        flaky_evidence.flaky = true;
        let raw_results = [
            spanned([10, 20]),
            spanned([15, 40]),
            spanned([5, 8]),
            failed,
            flaky,
            record("d", "test", None, &[]),
            evidence,
            flaky_evidence,
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
                ("e", None, false, None, None),
            ]
        );
        assert!(plan.mutants.is_empty());
        // A skipped test runs where it is Python's (mutmut runs it), and not
        // under another runner.
        let skipped = |id: &str, runner: &str| {
            let mut raw = record(id, "test", None, &[]);
            raw.status = Some("skipped".into());
            raw.provenance.runner = runner.into();
            raw
        };
        // A failed Python test, and a flaky skipped one, do not.
        let mut failed = skipped("f", "pytest");
        failed.status = Some("failed".into());
        let mut flaky = skipped("k", "pytest");
        flaky.flaky = true;
        let skips = super::plan(
            &[
                skipped("p", "pytest"),
                skipped("u", "unittest"),
                skipped("v", "vitest"),
                failed,
                flaky,
            ],
            listed(Vec::new(), &[]),
        )
        .unwrap();
        assert_eq!(
            skips
                .tests
                .iter()
                .map(|test| (test.id.as_str(), test.runnable))
                .collect::<Vec<_>>(),
            [
                ("p", true),
                ("u", true),
                ("v", false),
                ("f", false),
                ("k", false)
            ]
        );
    }

    /// Each mutant's candidates are the tests whose own evidence, with what
    /// their file's modules did as they loaded, may infect it: module-level
    /// code takes every test of the file it ran in, a setup no file owns
    /// takes every test, and a mutant no passing test may infect survives.
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

        // A test file's modules ran it, and the file has no test.
        let nobody = planned(&plan, "nobody");
        assert_eq!(
            (nobody.decided, nobody.reason.as_deref()),
            (Some(Decided::NoCoverage), Some("no-test-ran-it"))
        );
        assert!(nobody.candidates.is_empty());
        // A setup that names no file (Mocha loads every test file before
        // its first test): every test of the run.
        let loose = planned(&plan, "loose");
        assert_eq!(loose.decided, None);
        assert!(
            loose
                .candidates
                .iter()
                .all(|candidate| candidate.reason == CandidateReason::Unattributed)
                && loose.candidates.len() == plan.tests.iter().filter(|test| test.runnable).count(),
            "{loose:?}"
        );
        assert!(loose.whole_files.is_empty());
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
    /// candidate: a run of none has no candidate, and no test ran such a
    /// mutant, nor one whose function no record ran: no coverage.
    #[test]
    fn a_mutant_only_uncredited_work_ran_has_no_coverage_in_a_run_without_tests() {
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
        let none = Some(Decided::NoCoverage);
        let reason = Some("no-test-ran-it");
        assert_eq!(
            outcomes,
            [
                ("bg", "infected", none, reason, 0),
                ("nopoint", "unknown", none, reason, 0),
                ("unhit", "unknown", none, reason, 0),
            ]
        );
    }

    /// A mutant only failing tests ran: they ran it, and kill nothing.
    #[test]
    fn a_mutant_only_failing_tests_ran_survives() {
        let mut failed = record("f", "test", Some("t/f.test.js"), &["s"]);
        failed.status = Some("failed".into());
        let mutants = vec![mutant(serde_json::json!({"id": "m", "statement": "s"}))];
        let plan = plan(&[failed], listed(mutants, &[])).unwrap();
        let m = planned(&plan, "m");
        assert_eq!(
            (m.decided, m.reason.as_deref(), m.candidates.len()),
            (Some(Decided::Survived), Some("no-passing-candidate"), 0)
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
            could_infect(&method, &hits(&["s"]), &hits(&["s", "f"]), "javascript"),
            None
        );
        assert_eq!(
            could_infect(&method, &hits(&["f"]), &hits(&["f"]), "javascript"),
            None
        );
        assert_eq!(
            could_infect(&method, &hits(&["f"]), &hits(&["s", "f"]), "javascript"),
            Some(CandidateReason::Infected)
        );
        // A Python mutant's statement is inside its function: a test that
        // called the function without running the statement did not reach
        // it.
        assert_eq!(
            could_infect(&method, &hits(&["f"]), &hits(&["s", "f"]), "python"),
            None
        );
    }

    /// Whether a server ran a mutant's code: what tells the driver an arrow
    /// mutated whole at a module's top level is evaluated in a server as it
    /// loads, not in a page.
    #[test]
    fn a_mutant_whose_code_a_server_ran_says_so() {
        let arrow = mutant(serde_json::json!({"statement": "s", "function": "f"}));
        let mut raw = record("a", "test", None, &["f"]);
        assert!(!server_ran(&server_runs(&raw), &arrow));
        raw.server = vec![server_hit("f", None)];
        assert!(server_ran(&server_runs(&raw), &arrow));
        assert_eq!(server_wide(&raw, &server_runs(&raw), &arrow), None);
    }

    /// Work for a request that named no test, left no test's: sent while
    /// tests ran, one of them may have sent it without its name, and the
    /// mutant runs with the server started with it; sent while none ran (a
    /// readiness check), each request's own mutant covers it.
    #[test]
    fn a_request_no_test_named_sent_while_tests_ran_needs_the_server_started_with_it() {
        let in_function = mutant(serde_json::json!({"statement": "s", "function": "f"}));
        let mut background = record("background", "background", None, &[]);
        background.server = vec![server_hit(
            "s",
            Some(crate::coverage_report::REQUEST_OUTSIDE_TESTS),
        )];
        assert_eq!(
            server_wide(&background, &server_runs(&background), &in_function),
            None
        );
        background.server.push(server_hit(
            "s",
            Some(crate::coverage_report::REQUEST_DURING_TESTS),
        ));
        assert_eq!(
            server_wide(&background, &server_runs(&background), &in_function),
            Some(ServerWide::ByTime)
        );
    }

    /// A server module's top level that a request naming no test ran, while
    /// no test of its server ran: the module made it once, for every later
    /// request, so the mutant needs the server started with it.
    #[test]
    fn module_level_code_a_request_no_test_named_ran_needs_the_server_started_with_it() {
        let module_level = mutant(serde_json::json!({"statement": "s"}));
        let mut background = record("background", "background", None, &[]);
        background.server = vec![server_hit(
            "s",
            Some(crate::coverage_report::REQUEST_OUTSIDE_TESTS),
        )];
        assert_eq!(
            server_wide(&background, &server_runs(&background), &module_level),
            Some(ServerWide::ModuleLevel)
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
            server_wide(&raw, &server_runs(&raw), &module_level),
            Some(ServerWide::ModuleLevel)
        );
    }

    /// Planning reads each mutant from the records that ran its code, and
    /// costs about that: not, for each of them, the size of the record. A
    /// merged run of shards that each measured a whole browser suite holds
    /// thousands of records, each with thousands of hits and server records,
    /// and 65,092 mutants; reading each mutant's verdict from a copy of every
    /// such record's observations (its method's statement added) and from a
    /// scan of its server records took that merge over 12 minutes. Here 600
    /// mutants and 200 records of 2,000 hits and 1,000 server records each:
    /// 26 s that way in a debug build, and half a second now.
    #[test]
    fn planning_costs_each_mutant_s_records_and_not_their_size() {
        const MUTANTS: usize = 600;
        const RECORDS: usize = 200;
        let functions = (0..MUTANTS).map(|i| format!("f{i}")).collect::<Vec<_>>();
        let statements = (0..MUTANTS).map(|i| format!("s{i}")).collect::<Vec<_>>();
        // Each test ran every method and none of the statements around
        // them, which the run ran as its modules loaded, outside any test.
        let hits = functions
            .iter()
            .cloned()
            .chain((0..2_000 - MUTANTS).map(|i| format!("other{i}")))
            .collect::<Vec<_>>();
        let hits = hits.iter().map(String::as_str).collect::<Vec<_>>();
        let mut raw_results = (0..RECORDS)
            .map(|i| {
                let mut raw = record(&format!("t{i}"), "test", Some("t/a.test.js"), &hits);
                raw.server = (0..1_000)
                    .map(|j| server_hit(&format!("served{j}"), Some(1)))
                    .collect();
                raw
            })
            .collect::<Vec<_>>();
        let loaded = statements.iter().map(String::as_str).collect::<Vec<_>>();
        raw_results.push(record("load", "background", None, &loaded));
        let mutants = (0..MUTANTS)
            .map(|i| {
                mutant(serde_json::json!({
                    "id": format!("m{i}"),
                    "statement": statements[i],
                    "function": functions[i],
                }))
            })
            .collect::<Vec<_>>();
        let started = std::time::Instant::now();
        let plan = plan(&raw_results, listed(mutants, &[])).unwrap();
        let took = started.elapsed();
        assert_eq!(plan.mutants.len(), MUTANTS);
        // Every test ran each method, and its statement counts as run there.
        assert!(plan.mutants.iter().all(|mutant| mutant.covering == RECORDS));
        assert!(
            took < std::time::Duration::from_secs(10),
            "planning {MUTANTS} mutants over {RECORDS} records took {took:?}"
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

    /// A mutant of `costed`: its id, its candidates (`None`: decided) and the
    /// files it runs whole.
    type Costed<'a> = (&'a str, Option<&'a [usize]>, &'a [&'a str]);

    /// A plan of `tests` (file, duration) whose mutants are as given.
    fn costed(tests: &[(Option<&str>, Option<u64>)], mutants: &[Costed]) -> StrongPlan {
        StrongPlan {
            mutant_set: "set".into(),
            tests: tests
                .iter()
                .enumerate()
                .map(|(index, (file, duration))| PlannedTest {
                    id: format!("t{index}"),
                    file: file.map(str::to_owned),
                    name: format!("t{index}"),
                    title: None,
                    runner: "vitest".into(),
                    duration_ms: *duration,
                    span: None,
                    runnable: true,
                })
                .collect(),
            mutants: mutants
                .iter()
                .map(|(id, candidates, whole)| PlannedMutant {
                    id: (*id).into(),
                    file: "src/a.js".into(),
                    language: "javascript".into(),
                    index: 0,
                    mutator: "M".into(),
                    weak: if candidates.is_some() {
                        "infected"
                    } else {
                        "not-infected"
                    },
                    decided: candidates.is_none().then_some(Decided::Survived),
                    reason: None,
                    candidates: candidates
                        .unwrap_or_default()
                        .iter()
                        .map(|test| Candidate {
                            test: *test,
                            reason: CandidateReason::Infected,
                        })
                        .collect(),
                    whole_files: whole.iter().map(|file| (*file).to_owned()).collect(),
                    server_wide: None,
                    server_ran: false,
                    passed_before: 0,
                    covering: 0,
                    statement: None,
                    function: None,
                    function_hit_by: BTreeMap::new(),
                    narrowed: None,
                })
                .collect(),
        }
    }

    /// A mutant costs what its copies would if it survived: each file's
    /// start and its tests' measured times, an untimed test a second, every
    /// test of a file it runs whole, and a copy for candidates with no
    /// file. The cheapest run first, the plan's order between equals.
    #[test]
    fn a_mutant_costs_its_copies_and_the_cheapest_run_first() {
        let mut plan = costed(
            &[
                (Some("a.test.js"), Some(10)),
                (Some("a.test.js"), Some(20)),
                (Some("b.test.js"), None),
                (None, Some(5)),
            ],
            &[
                ("whole", Some(&[0]), &["a.test.js"]),
                ("two-files", Some(&[0, 2]), &[]),
                ("decided", None, &[]),
                ("one", Some(&[1]), &[]),
                ("fileless", Some(&[3]), &[]),
                ("also-one", Some(&[1]), &[]),
                ("gone-file", Some(&[]), &["c.test.js"]),
            ],
        );
        let costs = plan.estimated_ms();
        assert_eq!(costs["whole"], COPY_START_MS + 30);
        assert_eq!(costs["two-files"], 2 * COPY_START_MS + 10 + UNTIMED_TEST_MS);
        assert_eq!(costs["decided"], 0);
        assert_eq!(costs["fileless"], COPY_START_MS + 5);
        assert_eq!(costs["gone-file"], COPY_START_MS);
        plan.cheapest_first();
        let order = plan
            .mutants
            .iter()
            .map(|m| m.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            order,
            [
                "decided",
                "gone-file",
                "fileless",
                "one",
                "also-one",
                "whole",
                "two-files"
            ]
        );
    }

    /// The shares split what runs by what each mutant runs, dealt back and
    /// forth from the costliest: every share gets its costly mutants, each
    /// mutant is in one share, and the decided ones are the first share's.
    #[test]
    fn shares_split_the_mutants_by_what_they_run() {
        let tests = (0..8)
            .map(|index| {
                (
                    Some(if index < 4 { "a.test.js" } else { "b.test.js" }),
                    Some(1),
                )
            })
            .collect::<Vec<_>>();
        let all: [&[usize]; 7] = [
            &[0],
            &[0, 1, 2, 3, 4, 5, 6, 7],
            &[1],
            &[0, 1, 2, 3],
            &[2],
            &[0, 4],
            &[3],
        ];
        let ids = ["m0", "m1", "m2", "m3", "m4", "m5", "m6"];
        let mut mutants = ids
            .iter()
            .zip(all)
            .map(|(id, candidates)| (*id, Some(candidates), &[] as &[&str]))
            .collect::<Vec<_>>();
        mutants.insert(2, ("decided", None, &[]));
        let plan = costed(&tests, &mutants);
        let share = |k: usize| {
            let mut part = plan.clone();
            part.keep_share(k, 3);
            part.mutants
                .iter()
                .map(|mutant| mutant.id.clone())
                .collect::<Vec<_>>()
        };
        // Weights (copies and tests): m1 10, m3 5, m5 4, then 2 each. Dealt
        // m1 m3 m5 to shares 1 2 3, then m0 m2 m4 to 3 2 1, then m6 to 1.
        assert_eq!(share(1), ["m1", "decided", "m4", "m6"]);
        assert_eq!(share(2), ["m2", "m3"]);
        assert_eq!(share(3), ["m0", "m5"]);
        // One share is the whole plan.
        let mut whole = plan.clone();
        whole.keep_share(1, 1);
        assert_eq!(whole.mutants.len(), plan.mutants.len());
    }

    /// A plan with no test and every mutant decided (a run that measured
    /// nothing runnable) deals nothing: the decided mutants stay with the
    /// first share and the others are empty.
    #[test]
    fn a_plan_with_nothing_to_run_keeps_its_decided_mutants_in_the_first_share() {
        let plan = costed(&[], &[("decided", None, &[])]);
        let share = |k: usize| {
            let mut part = plan.clone();
            part.keep_share(k, 2);
            part.mutants
                .iter()
                .map(|mutant| mutant.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(share(1), ["decided"]);
        assert!(share(2).is_empty());
    }
}
