//! Run-owned assertion map lifecycle and execution-backed, agent-assessed score.
use crate::{
    assertion_inputs::ARCHIVE_PATH,
    assertion_map::{self as model, *},
    coverage_report::{
        ArchiveReportRequest, CoverageReport, ExitCodeInput, analyze_coverage_archive,
    },
    evidence_archive::read_archive,
    lifecycle::atomic_write,
    run_store::{RunMetadata, StoredRun, discover_runs},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

pub const MAP_FILE: &str = "assertions.json";
pub const STATE_FILE: &str = "assertions.state.json";
const REPORT_CACHE_FILE: &str = "assertions.report.cache.json";
pub struct RunManifest {
    pub manifest: InputManifest,
    pub evidence_digest: String,
    legacy_digest: Option<String>,
    pub statement_exclusions: Vec<Value>,
}
pub struct RunInputs {
    pub inputs: Inputs,
    pub stored: RunManifest,
}
impl std::ops::Deref for RunInputs {
    type Target = RunManifest;
    fn deref(&self) -> &RunManifest {
        &self.stored
    }
}
pub fn load_inputs(root: &Path, run: &StoredRun) -> Result<RunInputs, String> {
    let stored = load_manifest(run)?;
    let inputs = crate::assertion_inputs::current_sources(root, &stored.manifest)?;
    Ok(RunInputs { inputs, stored })
}
pub fn load_manifest(run: &StoredRun) -> Result<RunManifest, String> {
    load_optional_manifest(run)?.ok_or_else(|| {
        "This older run has no assertion manifest. Run tests once with this version of Supercov"
            .into()
    })
}
fn load_optional_manifest(run: &StoredRun) -> Result<Option<RunManifest>, String> {
    if run.metadata.merged == Some(true) {
        return Err(
            "Use a single run for assertion maps; merged runs have multiple input manifests".into(),
        );
    }
    let bytes = fs::read(&run.evidence_path).map_err(|e| e.to_string())?;
    let evidence_digest = format!("{:x}", Sha256::digest(&bytes));
    let entries = read_archive(&run.evidence_path).map_err(|e| e.to_string())?;
    let Some(input) = entries.iter().find(|e| e.path == ARCHIVE_PATH) else {
        return Ok(None);
    };
    let value: Value = serde_json::from_slice(&input.contents).map_err(|e| e.to_string())?;
    let (manifest, legacy_digest) = match value["schemaVersion"].as_u64() {
        Some(2) => (
            serde_json::from_value::<InputManifest>(value).map_err(|e| e.to_string())?,
            None,
        ),
        Some(1) => {
            // Import old maps without requiring their former checkout. Legacy
            // sources are used only to obtain hashes, never as today's source.
            let legacy: Inputs = serde_json::from_value(value).map_err(|e| e.to_string())?;
            if legacy
                .assertions
                .iter()
                .any(|s| s.at.offset(&legacy.files).is_none())
            {
                return Err("Invalid legacy assertion inputs".into());
            }
            (legacy.manifest(), Some(digest(&legacy)))
        }
        _ => return Err("Unsupported assertion input schema".into()),
    };
    if manifest.files.iter().any(|(p, f)| {
        !local_path(p) || f.sha256.len() != 64 || !f.sha256.bytes().all(|b| b.is_ascii_hexdigit())
    }) || manifest.assertions.iter().any(|s| {
        !manifest.files.contains_key(&s.at.file)
            || s.at.line == 0
            || s.at.column == 0
            || s.at.text.is_empty()
    }) {
        return Err("Invalid assertion input manifest".into());
    }
    if fs::read(&run.evidence_path).map_err(|e| e.to_string())? != bytes {
        return Err("Run archive changed during read".into());
    }
    Ok(Some(RunManifest {
        manifest,
        evidence_digest,
        legacy_digest,
        statement_exclusions: entries
            .iter()
            .find(|e| e.path == "statement-exclusions.json")
            .map(|entry| serde_json::from_slice(&entry.contents).map_err(|e| e.to_string()))
            .transpose()?
            .unwrap_or_default(),
    }))
}
pub fn load(run: &StoredRun, input: &RunManifest) -> Result<(AssertionMap, State), String> {
    let read = |file: &str| {
        fs::read(run.directory.join(file))
            .map_err(|e| format!("{file}: {e}; new test runs create assertion maps automatically"))
    };
    let map = model::parse_stored(&read(MAP_FILE)?).map_err(|e| format!("{MAP_FILE}: {e}"))?;
    let state = model::parse_state(
        &read(STATE_FILE)?,
        &map,
        &input.manifest,
        &input.evidence_digest,
        input.legacy_digest.as_deref(),
    )?;
    Ok((map, state))
}
fn write_json(
    root: &Path,
    run: &StoredRun,
    name: &str,
    value: &impl serde::Serialize,
) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    atomic_write(root, &run.directory.join(name), &bytes).map_err(|e| e.to_string())
}
/// Create the map inside the unpublished run directory. The lifecycle publishes
/// evidence, map and review state together with one directory rename. Older
/// archives without assertion manifests and merged runs retain their existing behavior.
pub(crate) fn prepare_publication(
    root: &Path,
    directory: &Path,
    metadata: &RunMetadata,
) -> Result<(), String> {
    if metadata.merged == Some(true) {
        return Ok(());
    }
    let run = StoredRun {
        id: metadata.id.clone(),
        directory: directory.into(),
        evidence_path: directory.join("evidence.raw.gz"),
        metadata_path: directory.join("run.json"),
        query_index_path: directory.join(crate::run_store::RUST_QUERY_INDEX_FILE),
        metadata: metadata.clone(),
    };
    let Some(input) = load_optional_manifest(&run)? else {
        return Ok(());
    };
    // Refuse replacement even if this helper is accidentally called twice.
    if directory.join(MAP_FILE).exists() || directory.join(STATE_FILE).exists() {
        return Err("Refusing to replace an existing assertion map or review state".into());
    }
    let current = crate::assertion_inputs::current_sources(root, &input.manifest);
    let inventory = discover_runs(root).map_err(|e| e.to_string())?;
    let mut inheritance = Inheritance::default();
    let mut inherited = None;
    for previous in &inventory.runs {
        if previous.id == run.id
            || previous.metadata.merged == Some(true)
            || previous.metadata.command != metadata.command
            || (!previous.directory.join(MAP_FILE).exists()
                && !previous.directory.join(STATE_FILE).exists())
        {
            continue;
        }
        let attempt = (|| {
            let old = load_manifest(previous)?;
            if old.manifest.language != input.manifest.language {
                return Ok(None);
            }
            let (map, state) = load(previous, &old)?;
            let a = &previous.metadata.integrity.fingerprint;
            let b = &metadata.integrity.fingerprint;
            // Source hashes are checked through anchors/watches by carry. Only
            // execution context changes invalidate every inherited flow.
            let context_changed = a.configuration != b.configuration
                || a.dependencies != b.dependencies
                || a.instrumenter != b.instrumenter
                || old.manifest.context_digest != input.manifest.context_digest;
            let current = match &current {
                Ok(current) => current,
                Err(reason) => {
                    // Publish the run even if files were edited during testing.
                    // Preserve authored work as suggestions instead of guessing
                    // locations in a checkout that no longer matches this run.
                    let (mut next, mut next_state) =
                        seed_manifest(&input.manifest, &input.evidence_digest);
                    next.retired_assertions = map.retired_assertions;
                    for assertion in map.assertions {
                        if let Some(site) =
                            next.assertions.iter_mut().find(|a| a.at == assertion.at)
                        {
                            *site = assertion;
                        } else {
                            next.retired_assertions.push(Retired {
                                assertion,
                                reason: reason.clone(),
                            });
                        }
                    }
                    let mut reserved = next
                        .retired_assertions
                        .iter()
                        .map(|r| r.assertion.id.clone())
                        .chain(
                            next.assertions
                                .iter()
                                .filter(|a| !a.flows.is_empty())
                                .map(|a| a.id.clone()),
                        )
                        .collect::<BTreeSet<_>>();
                    for assertion in next.assertions.iter_mut().filter(|a| a.flows.is_empty()) {
                        while !reserved.insert(assertion.id.clone()) {
                            assertion.id.push('_');
                        }
                    }
                    invalidate(&mut next_state, &next, reason);
                    add_change(
                        &mut next_state,
                        None,
                        None,
                        None,
                        reason.clone(),
                        BTreeSet::new(),
                    );
                    return Ok(Some((next, next_state)));
                }
            };
            model::carry(
                &map,
                &state,
                &old.manifest,
                current,
                &input.evidence_digest,
                context_changed,
            )
            .map(Some)
        })();
        match attempt {
            Ok(Some(pair)) => {
                inheritance.from = Some(previous.id.clone());
                inherited = Some(pair);
                break;
            }
            Ok(None) => (),
            Err(reason) => inheritance.skipped.push(SkippedMap {
                run: previous.id.clone(),
                reason,
            }),
        }
    }
    let (map, mut state) =
        inherited.unwrap_or_else(|| seed_manifest(&input.manifest, &input.evidence_digest));
    // A malformed newer map might contain changed claims. An older fallback
    // preserves work, but must not silently restore its previous credit.
    if !inheritance.skipped.is_empty() {
        invalidate(
            &mut state,
            &map,
            "newer assertion map could not be reused; inspect inherited claims",
        );
    }
    if let Err(reason) = current {
        invalidate(&mut state, &map, &reason);
        add_change(&mut state, None, None, None, reason, BTreeSet::new());
    }
    state.inheritance = Some(inheritance);
    write_json(root, &run, MAP_FILE, &map)?;
    write_json(root, &run, STATE_FILE, &state)?;
    Ok(())
}
pub fn coverage(run: &StoredRun) -> Result<CoverageReport, String> {
    analyze_coverage_archive(&ArchiveReportRequest {
        archive_path: run.evidence_path.clone(),
        run_id: run.id.clone(),
        generated_at: run.metadata.started_at.clone(),
        integrity: None,
        test_exit_code: ExitCodeInput::Present(run.metadata.test_exit_code),
    })
    .map_err(|e| format!("{e:?}"))
}
fn byte_column(source: &str, line: usize, column: usize, language: &str) -> Option<usize> {
    if line == 0 {
        return None;
    }
    let line = source.lines().nth(line - 1)?;
    if language != "javascript" {
        // Native Rust, Python and Ruby manifests use zero-based byte columns.
        return column.checked_add(1);
    }
    if column == 0 {
        return None;
    }
    let mut units = 0;
    for (byte, ch) in line.char_indices() {
        if units == column - 1 {
            return Some(byte + 1);
        }
        units += ch.len_utf16();
    }
    (units == column - 1).then_some(line.len() + 1)
}
fn phase_location<'a>(
    phase: &'a crate::coverage_report::CoveragePhase,
    inputs: &Inputs,
) -> Option<(&'a str, usize, usize)> {
    if phase.kind != "assertion" || phase.status.as_deref() != Some("passed") {
        return None;
    }
    let source = phase
        .operation
        .strip_prefix("Rust assertion at ")
        .or(phase.source.as_deref());
    let location = source?;
    let mut parts = location.rsplitn(3, ':');
    let column = parts.next()?.parse::<usize>().ok()?;
    let line = parts.next()?.parse::<usize>().ok()?;
    let file = parts.next()?;
    let column = byte_column(inputs.files.get(file)?, line, column, &inputs.language)?;
    Some((file, line, column))
}

/// Cache derived assessments separately from immutable coverage evidence.
/// Every query still verifies current source, map and managed-state identities.
pub fn report(root: &Path, run: &StoredRun) -> Result<Value, String> {
    report_with_detail(root, run, None)
}
/// Read authored flows and their assessment from the same map snapshot.
pub fn assertion(root: &Path, run: &StoredRun, id: &str) -> Result<Value, String> {
    report_with_detail(root, run, Some(id))
}
fn report_with_detail(root: &Path, run: &StoredRun, id: Option<&str>) -> Result<Value, String> {
    let input = load_inputs(root, run)?;
    let (map, state) = load(run, &input)?;
    let cache_key = digest(&(
        env!("SUPERCOV_ENGINE_SOURCE_SHA256"),
        &run.id,
        run.metadata.test_exit_code,
        &map,
        &state,
        &input.evidence_digest,
    ));
    let cache_path = run.directory.join(REPORT_CACHE_FILE);
    let mut report = read_report_cache(&cache_path, &cache_key).unwrap_or_else(|| Value::Null);
    if report.is_null() {
        let coverage = coverage(run)?;
        report = assess(
            &map,
            &state,
            &input.inputs,
            &coverage,
            run.metadata.test_exit_code == Some(0),
        );
        report["excludedStatements"] = json!(input.statement_exclusions);
        report["summary"]["excludedStatements"] = json!(input.statement_exclusions.len());
        // This is disposable acceleration. A read-only directory or corrupt
        // cache must never prevent a freshly computed report from working.
        let cached = json!({"key":cache_key,"digest":digest(&report),"report":report});
        let _ = write_json(root, run, REPORT_CACHE_FILE, &cached);
    }
    if let Some(id) = id {
        let matches = report["assertions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["id"] == id)
            .collect::<Vec<_>>();
        let mut row = match matches.as_slice() {
            [row] => (*row).clone(),
            [] => {
                return Err(format!(
                    "Unknown assertion ID: {id}; use runs {} assertions to list IDs",
                    run.id
                ));
            }
            _ => {
                return Err(format!(
                    "Ambiguous assertion ID: {id}; repair duplicate IDs in assertions.json"
                ));
            }
        };
        if let Some(authored) = map.assertions.iter().find(|a| a.id == id) {
            for (flow, authored) in row["flows"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .zip(&authored.flows)
            {
                let assessment = flow.as_object().unwrap().clone();
                *flow = serde_json::to_value(authored).map_err(|e| e.to_string())?;
                flow.as_object_mut().unwrap().extend(assessment);
            }
        }
        report["assertion"] = row;
    }
    report["inheritance"] = json!(state.inheritance);
    report["revision"] = json!(digest(&(&map, &state, &input.evidence_digest)));
    Ok(report)
}

fn read_report_cache(path: &Path, key: &str) -> Option<Value> {
    if fs::metadata(path).ok()?.len() > 256 * 1024 * 1024 {
        return None;
    }
    let cached: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let report = &cached["report"];
    (cached["key"] == key
        && cached["digest"] == digest(report)
        && report["summary"].is_object()
        && report["assertions"].is_array())
    .then(|| report.clone())
}

pub fn assess(
    map: &AssertionMap,
    state: &State,
    inputs: &Inputs,
    coverage: &CoverageReport,
    passed: bool,
) -> Value {
    let manifest = inputs.manifest();
    let validation = model::validation(map, state, inputs);
    let errors = validation["errors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    let pending_changes = validation["changes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["current"] != true)
        .count();
    let view = &coverage.filters.passed;
    let tests = view
        .tests
        .iter()
        .filter(|t| t.role == "test")
        .map(|t| (&t.id, t))
        .collect::<BTreeMap<_, _>>();
    let measured_statements = view
        .points
        .iter()
        .filter(|p| p.measured && p.meta.kind == crate::coverage_analysis::PointKind::Statement)
        .collect::<Vec<_>>();
    let all_points = coverage
        .view
        .points
        .iter()
        .map(|p| (&p.meta.id, p))
        .collect::<BTreeMap<_, _>>();
    let all_tests = coverage
        .view
        .tests
        .iter()
        .map(|t| (&t.id, t))
        .collect::<BTreeMap<_, _>>();
    let mut by_file = BTreeMap::<&str, Vec<(usize, &crate::coverage_report::PointResult)>>::new();
    let mut by_line = BTreeMap::<(String, usize), Vec<&crate::coverage_report::PointResult>>::new();
    for point in &measured_statements {
        by_line
            .entry((point.meta.file.clone(), point.meta.line))
            .or_default()
            .push(point);
        if let Some(text) = inputs.files.get(&point.meta.file)
            && let Some(column) =
                byte_column(text, point.meta.line, point.meta.column, &inputs.language)
        {
            let at = Anchor {
                file: point.meta.file.clone(),
                line: point.meta.line,
                column,
                text: point.meta.source.clone(),
            };
            if let Some(start) = at.offset(&inputs.files) {
                by_file
                    .entry(&point.meta.file)
                    .or_default()
                    .push((start, point));
            }
        }
    }
    for points in by_file.values_mut() {
        points.sort_by_key(|(pos, _)| *pos);
    }
    let mut rows = Vec::new();
    let mut claimed_points = BTreeSet::new();
    let mut credited_points = BTreeSet::new();
    let mut point_flows = BTreeMap::<String, BTreeSet<String>>::new();
    let mut draft_flows = 0;
    let mut stale_flows = 0;
    let mut invalid_flows = 0;
    let mut current_flows = 0;
    let mut credit_flows = 0;
    let mut line_assertions = BTreeMap::<(String, usize), BTreeSet<String>>::new();
    // Duplicate identities invalidate all credit; stale anchors only invalidate
    // their own flow, so an agent can repair a large map incrementally.
    let identities_valid = state.inputs_digest == inputs.identity()
        && errors.iter().all(|e| {
            !e.contains("duplicate") && !e.contains("schema") && !e.contains("assertion ID")
        });
    // Join exact identities once, rather than rescanning the complete inventory
    // for every assertion/phase pair. This is evidence lookup, not inference.
    let mut phases_by_location = BTreeMap::new();
    for p in &view.phases {
        if !tests.contains_key(&p.test) {
            continue;
        }
        if let Some(location) = phase_location(&p.phase, inputs) {
            phases_by_location
                .entry(location)
                .or_insert_with(Vec::new)
                .push(p);
        }
    }
    let mut inventory = BTreeMap::<&Anchor, BTreeSet<&str>>::new();
    for site in &inputs.assertions {
        inventory
            .entry(&site.at)
            .or_default()
            .insert(&site.operation);
    }
    let mapped_anchors = map
        .assertions
        .iter()
        .map(|a| &a.at)
        .collect::<BTreeSet<_>>();
    let mut used_ids = map
        .assertions
        .iter()
        .map(|a| a.id.clone())
        .chain(
            map.retired_assertions
                .iter()
                .map(|r| r.assertion.id.clone()),
        )
        .collect::<BTreeSet<_>>();
    let missing = seed(inputs, "")
        .0
        .assertions
        .into_iter()
        .filter(|a| !mapped_anchors.contains(&a.at))
        .map(|mut a| {
            while !used_ids.insert(a.id.clone()) {
                a.id.push('_');
            }
            a
        })
        .collect::<Vec<_>>();
    for (a, in_map) in map
        .assertions
        .iter()
        .map(|a| (a, true))
        .chain(missing.iter().map(|a| (a, false)))
    {
        let witnesses = phases_by_location
            .get(&(a.at.file.as_str(), a.at.line, a.at.column))
            .into_iter()
            .flatten()
            // Coordinates alone cannot identify a JS assertion: retain the
            // complete inventoried expression and operation requirement.
            .filter(|p| {
                inputs.language != "javascript"
                    || inventory
                        .get(&a.at)
                        .is_some_and(|operations| operations.contains(p.phase.operation.as_str()))
            })
            .map(|p| p.test.clone())
            .collect::<BTreeSet<_>>();
        let mut flows = Vec::new();
        for f in &a.flows {
            let dirty = model::reasons_for_manifest(a, f, map, state, inputs, &manifest);
            let valid = model::validate_flow(f, &inputs.files).is_empty()
                && a.at.offset(&inputs.files).is_some();
            let freshness = if f.basis.is_none() {
                draft_flows += 1;
                "draft"
            } else if f.basis.as_deref()
                != Some(model::expected_basis(a, f, map, state, &manifest).as_str())
            {
                stale_flows += 1;
                "stale"
            } else {
                "current"
            };
            if !valid {
                invalid_flows += 1;
            }
            if dirty.is_empty() {
                current_flows += 1;
            }
            // A file + exact displayed name must resolve to one logical test.
            // Retry attempts remain under that identity; duplicate names do not.
            let mut resolved = BTreeSet::new();
            let mut selectors = Vec::new();
            for selector in &f.applies_to {
                let matches = coverage
                    .view
                    .tests
                    .iter()
                    .filter(|t| {
                        t.file.as_deref() == Some(selector.file.as_str()) && t.name == selector.name
                    })
                    .collect::<Vec<_>>();
                let status = match matches.as_slice() {
                    [test] if witnesses.contains(&test.id) && tests.contains_key(&test.id) => {
                        resolved.insert(test.id.clone());
                        "observed"
                    }
                    [] => "missing",
                    [_] => "unobserved",
                    _ => "ambiguous",
                };
                selectors.push(json!({"file":selector.file,"name":selector.name,"status":status,
                    "outcomes":matches.iter().map(|t| &t.outcome).collect::<Vec<_>>(),
                    "reason":match matches.as_slice() {
                        [] => "No test execution record matches this selector. The test may be disabled or outside this run.",
                        [test] if test.outcome != "passed" => "The selected test has no passing outcome.",
                        [_] if status == "unobserved" => "The test passed but this assertion occurrence was not recorded. Its branch may not have run, or attribution may be missing.",
                        [_] => "A passing assertion occurrence matches this test.",
                        _ => "Multiple test identities match this selector."
                    }}));
            }
            let applicable = resolved;
            let eligible = passed && identities_valid && dirty.is_empty() && !applicable.is_empty();
            let mut blockers = Vec::new();
            if !passed {
                blockers.push("run did not pass");
            }
            if !identities_valid {
                blockers.push("invalid map identities");
            }
            if !dirty.is_empty() {
                blockers.push("flow requires review or reference repair");
            }
            if applicable.is_empty() {
                blockers.push("no matching passing assertion occurrence for appliesTo");
            }
            if eligible {
                credit_flows += 1;
            }
            let mut lines = BTreeSet::new();
            let mut node_credit = Vec::new();
            for node in &f.nodes {
                let claimed = f.counts_as_asserted.contains(&node.id);
                let start = node.at.offset(&inputs.files);
                let mut matched = Vec::new();
                if let Some(start) = start
                    && let Some(points) = by_file.get(node.at.file.as_str())
                {
                    let first = points.partition_point(|(pos, _)| *pos < start);
                    for (_, point) in points[first..].iter().take_while(|(pos, _)| *pos == start) {
                        // Exact statement identity only: a guard/block does not
                        // include its nested statements in the score or diagnostic.
                        if point.meta.source == node.at.text {
                            matched.push(*point);
                        }
                    }
                }
                let matching_tests = matched
                    .iter()
                    .filter(|p| p.covered)
                    .flat_map(|p| p.tests.iter())
                    .filter(|test| applicable.contains(*test))
                    .collect::<BTreeSet<_>>();
                let credited = claimed && eligible && !matching_tests.is_empty();
                let mut reasons = Vec::new();
                let mut reason = |code: &str, message: String| {
                    reasons.push(json!({"code":code,"message":message}));
                };
                if !claimed {
                    reason(
                        "context_only",
                        "The agent included this node as context, not in countsAsAsserted.".into(),
                    );
                } else if credited {
                    reason("same_test_execution", "Current agent claim, passing assertion, and statement execution in the same selected test.".into());
                } else {
                    if start.is_none() {
                        reason(
                            "invalid_source_anchor",
                            "The node's source anchor does not match the run's current source."
                                .into(),
                        );
                    } else if matched.is_empty() {
                        reason("no_measured_statement", "The node does not exactly identify a measured production statement in this run.".into());
                    }
                    if !passed {
                        reason("run_failed", "The test run did not pass.".into());
                    }
                    if !identities_valid {
                        reason("invalid_map_identity", "Map identities or managed input state are invalid; see validation errors.".into());
                    }
                    if !dirty.is_empty() {
                        reason(
                            "flow_needs_attention",
                            format!(
                                "Flow needs investigation: {}",
                                dirty.iter().cloned().collect::<Vec<_>>().join("; ")
                            ),
                        );
                    }
                    if applicable.is_empty() {
                        reason("no_passing_assertion", "No selected test has a matching passing occurrence of this assertion; see flow selectors.".into());
                    }
                    if !matched.is_empty() {
                        if !matched.iter().any(|p| p.covered) {
                            let executed = matched
                                .iter()
                                .filter_map(|p| all_points.get(&p.meta.id))
                                .filter(|p| p.covered)
                                .collect::<Vec<_>>();
                            if !executed.is_empty() {
                                let setup = executed
                                    .iter()
                                    .flat_map(|p| &p.tests)
                                    .any(|id| all_tests.get(id).is_some_and(|t| t.role == "setup"));
                                reason(if setup {"shared_setup_execution"} else {"execution_outside_passing_tests"},
                                    if setup {"Execution was recorded in a separate setup scope. Shared setup is not automatically credited to consuming tests."} else {"Execution was recorded outside passing tests (for example module initialization, background work or a failed test). It cannot establish same-test execution."}.into());
                            }
                            reason(
                                "no_passing_execution",
                                "No execution evidence from passing tests for this statement."
                                    .into(),
                            );
                        } else if !applicable.is_empty() && matching_tests.is_empty() {
                            if matched
                                .iter()
                                .flat_map(|p| &p.tests)
                                .any(|id| all_tests.get(id).is_some_and(|t| t.role == "setup"))
                            {
                                reason("shared_setup_execution", "Execution was recorded in a separate setup scope. Shared setup is not automatically credited to consuming tests.".into());
                            }
                            reason("no_same_test_execution", "No execution evidence attributed to a selected passing test for this assertion.".into());
                        }
                    }
                }
                node_credit.push(json!({
                    "nodeId":node.id,
                    "location":{"file":node.at.file,"line":node.at.line,"column":node.at.column},
                    "status":if !claimed {"context"} else if credited {"credited"} else {"notCredited"},
                    "statementIds":matched.iter().map(|p| &p.meta.id).collect::<Vec<_>>(),
                    "matchingTests":matching_tests,
                    "reasons":reasons,
                }));
                for point in matched.into_iter().filter(|_| claimed) {
                    claimed_points.insert(point.meta.id.clone());
                    if eligible
                        && point.covered
                        && point.tests.iter().any(|t| applicable.contains(t))
                    {
                        credited_points.insert(point.meta.id.clone());
                        point_flows
                            .entry(point.meta.id.clone())
                            .or_default()
                            .insert(flow_key(a, f));
                        lines.insert((node.at.file.clone(), point.meta.line));
                        line_assertions
                            .entry((node.at.file.clone(), point.meta.line))
                            .or_default()
                            .insert(a.id.clone());
                    }
                }
            }
            flows.push(json!({"id":f.id,"freshness":freshness,"valid":valid,"current":dirty.is_empty(),"expectedBasis":model::expected_basis(a,f,map,state,&manifest),"selectors":selectors,"questions":f.questions,"reasons":dirty,"eligible":eligible,"blockers":blockers,"matchingTests":applicable,"creditedStatementLines":lines,"nodeCredit":node_credit}));
        }
        let observation = if !witnesses.is_empty() {
            "Passing assertion occurrence recorded."
        } else if flows
            .iter()
            .flat_map(|f| f["selectors"].as_array().into_iter().flatten())
            .any(|s| s["status"] == "missing")
        {
            "No passing occurrence. Some selected tests have no execution record (for example disabled tests or tests outside this run)."
        } else if flows
            .iter()
            .flat_map(|f| f["selectors"].as_array().into_iter().flatten())
            .any(|s| {
                s["outcomes"]
                    .as_array()
                    .is_some_and(|outcomes| outcomes.iter().any(|o| o == "skipped"))
            })
        {
            "No passing occurrence. Selected tests include skipped/TODO executions."
        } else {
            "No passing occurrence recorded. The assertion may be in an untaken branch or its execution attribution may be missing; inspect the selected tests."
        };
        rows.push(json!({"observation":observation,"id":a.id,"at":a.at,"questions":a.questions,"inMap":in_map,"observes":a.observes,"operations":inventory.get(&a.at).cloned().unwrap_or_default(),"observedPassingTests":witnesses,"flows":flows}));
    }
    // Only lines carrying a measured statement can ever be asserted: credit is
    // claimed per statement, and statements are keyed by their anchor line. A
    // measured line without one — a continuation line of a multi-line
    // statement, or a nested function/arrow body with no statement of its own —
    // is unclaimable by construction, so counting it here would cap the
    // percentage below 100% for structural reasons and list it under
    // `unassertedLines` as if an agent could act on it.
    let denominator = coverage
        .view
        .lines
        .iter()
        .filter(|l| l.measured)
        .map(|l| (l.file.clone(), l.line))
        .filter(|location| by_line.contains_key(location))
        .collect::<BTreeSet<_>>();
    let mut credited = BTreeSet::new();
    let mut declared = BTreeSet::new();
    for location in &denominator {
        let statements = by_line.get(location).map(Vec::as_slice).unwrap_or(&[]);
        if !statements.is_empty()
            && statements
                .iter()
                .all(|p| claimed_points.contains(&p.meta.id))
        {
            declared.insert(location.clone());
        }
        if !statements.is_empty()
            && statements
                .iter()
                .all(|p| credited_points.contains(&p.meta.id))
        {
            credited.insert(location.clone());
        }
    }
    let missing_inventory = inputs
        .assertions
        .iter()
        .filter(|s| !map.assertions.iter().any(|a| a.at == s.at))
        .count();
    let (status, reason) = if !passed || !identities_valid {
        ("unavailable", "Run failed or map identities are invalid")
    } else if pending_changes > 0 {
        ("pending", "Source changes need impact assessment")
    } else if measured_statements.is_empty() {
        ("notApplicable", "No measured statements")
    } else if credit_flows > 0 {
        (
            "available",
            "Agent-assessed statements; mapping completeness is unknown",
        )
    } else if map.assertions.iter().all(|a| a.flows.is_empty()) {
        ("notAssessed", "No recorded flow explanations")
    } else {
        (
            "pending",
            "No current flow with matching passing assertion evidence",
        )
    };
    let assertions_without_current_explanation = rows
        .iter()
        .filter(|a| {
            inventory.keys().any(|at| json!(at) == a["at"])
                && a["observedPassingTests"]
                    .as_array()
                    .is_some_and(|v| !v.is_empty())
                && a["flows"]
                    .as_array()
                    .is_none_or(|v| !v.iter().any(|f| f["eligible"] == true))
        })
        .count();
    let total = denominator.len();
    let statements = measured_statements.iter().map(|p| {
        let at = inputs.files.get(&p.meta.file)
            .and_then(|text| byte_column(text, p.meta.line, p.meta.column, &inputs.language))
            .map(|column| Anchor { file: p.meta.file.clone(), line: p.meta.line, column, text: p.meta.source.clone() })
            .filter(|at| at.offset(&inputs.files).is_some());
        let all = all_points.get(&p.meta.id);
        json!({"id":p.meta.id,"file":p.meta.file,"line":p.meta.line,"at":at,"covered":p.covered,"tests":p.tests,"declared":claimed_points.contains(&p.meta.id),"asserted":credited_points.contains(&p.meta.id),"flows":point_flows.get(&p.meta.id).cloned().unwrap_or_default(),
            "executionEvidence":{"anyExecution":all.is_some_and(|p| p.covered),"passingTests":p.tests.iter().filter(|id| tests.contains_key(id)).collect::<Vec<_>>(),
                "outsidePassingTests":all.into_iter().flat_map(|p| &p.tests).filter(|id| !tests.contains_key(id)).collect::<Vec<_>>()}})
    }).collect::<Vec<_>>();
    json!({"basis":"agent-assessed; passing assertion identity and same-test execution required; not mutation resistance",
        "summary":{"status":status,"reason":reason,"pendingChanges":pending_changes,"metric":"measured statements","statements":{"asserted":credited_points.len(),"declared":claimed_points.len(),"total":measured_statements.len(),"percentage":if status != "available" { None } else {Some(credited_points.len() as f64 * 100.0 / measured_statements.len() as f64)}},"assertions":map.assertions.len(),"inventoryAssertions":inputs.assertions.len(),"missingInventoryAssertions":missing_inventory,
            "assertionsWithFlows":rows.iter().filter(|a| a["flows"].as_array().is_some_and(|f| !f.is_empty())).count(),
            "assertionsWithoutFlows":rows.iter().filter(|a| a["flows"].as_array().is_none_or(Vec::is_empty)).count(),
            "observedAssertionsWithoutCurrentExplanation":assertions_without_current_explanation,
            "questions":map.assertions.iter().map(|a| a.questions.len()+a.flows.iter().map(|f| f.questions.len()).sum::<usize>()).sum::<usize>(),
            "currentFlows":current_flows,"draftFlows":draft_flows,"staleFlows":stale_flows,"invalidFlows":invalid_flows,"eligibleFlows":credit_flows,"retiredAssertions":map.retired_assertions.len(),
            "unobservedAssertions":rows.iter().filter(|a| a["observedPassingTests"].as_array().is_none_or(Vec::is_empty)).count(),
            "inventoryFailures":inputs.limitations.iter().filter(|s| s.starts_with("Inventory unavailable for ")).count(),
            "unanchoredStatements":statements.iter().filter(|s| s["at"].is_null()).count(),
            "runPassed":passed,
            "lines":{"asserted":credited.len(),"declared":declared.len(),"total":total,"percentage":if total==0 || status != "available" {None} else {Some(credited.len() as f64 * 100.0 / total as f64)}}},
        "assertions":rows,"statements":statements,"tests":coverage.view.tests.iter().map(|t| json!({"id":t.id,"file":t.file,"name":t.name,"role":t.role,"outcome":t.outcome,"provenance":t.provenance,
            "hasExecutionEvidence":!t.hits.is_empty() || !t.decisions.is_empty() || !t.lines.is_empty()})).collect::<Vec<_>>(),
        "creditedLines":credited.iter().map(|loc| json!({"file":loc.0,"line":loc.1,"assertions":line_assertions.get(loc)})).collect::<Vec<_>>(),
        "unassertedLines":denominator.difference(&credited).map(|(f,l)| json!({"file":f,"line":l})).collect::<Vec<_>>(),
        "changes":validation["changes"],"validationErrors":errors,"limitations":inputs.limitations})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn report_cache_requires_exact_revision_and_intact_payload() {
        let directory =
            std::env::temp_dir().join(format!("supercov-report-cache-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(REPORT_CACHE_FILE);
        let report = json!({"summary":{"statements":{"asserted":4,"percentage":97.02842377260981}},"assertions":[]});
        let mut cache = json!({"key":"revision-one","digest":digest(&report),"report":report});
        fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
        assert_eq!(read_report_cache(&path, "revision-one"), Some(report));
        assert!(read_report_cache(&path, "revision-two").is_none());
        cache["report"]["summary"]["statements"]["asserted"] = json!(100);
        fs::write(&path, serde_json::to_vec(&cache).unwrap()).unwrap();
        assert!(read_report_cache(&path, "revision-one").is_none());
        fs::write(&path, "interrupted write").unwrap();
        assert!(read_report_cache(&path, "revision-one").is_none());
        fs::remove_dir_all(directory).unwrap();
    }
    use crate::evidence_archive::{EvidenceArchiveEntry, write_archive};

    #[test]
    fn legacy_maps_import_without_the_old_checkout_and_require_review() {
        let root = std::env::temp_dir().join(format!("supercov-legacy-map-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = "import assert from 'node:assert/strict'; assert.equal(1, 1);\n";
        fs::write(root.join("test.js"), source).unwrap();
        let old =
            crate::assertion_inputs::capture(&root, "javascript", ["test.js".into()]).unwrap();
        let directory = crate::run_store::create_analyzable_test_run(&root, "legacy");
        let path = directory.join("evidence.raw.gz");
        let mut entries = read_archive(&path).unwrap();
        entries.push(EvidenceArchiveEntry {
            path: ARCHIVE_PATH.into(),
            contents: serde_json::to_vec(&old).unwrap(),
        });
        let archive = write_archive(entries, &path).unwrap();
        let metadata_path = directory.join("run.json");
        let mut metadata: RunMetadata =
            serde_json::from_slice(&fs::read(&metadata_path).unwrap()).unwrap();
        metadata.raw_evidence.files = archive.files;
        metadata.raw_evidence.compressed_bytes = archive.compressed_bytes;
        metadata.raw_evidence.uncompressed_bytes = archive.uncompressed_bytes;
        fs::write(&metadata_path, serde_json::to_vec(&metadata).unwrap()).unwrap();
        let run = discover_runs(&root).unwrap().runs.remove(0);
        let stored = load_manifest(&run).unwrap();
        let (map, _) = seed(&old, &stored.evidence_digest);
        let legacy_map = json!({"schemaVersion":1,"assertions":[{"id":map.assertions[0].id,"at":map.assertions[0].at,"analysis":"mapped","observes":[],"flows":[{
            "id":"constant","explanation":"The assertion checks the constant one.","appliesTo":[],"nodes":[],"edges":[],"countsAsAsserted":[],"watch":[{"kind":"span","at":map.assertions[0].at}]
        }]}]});
        let legacy_state = json!({"schemaVersion":1,"inputsDigest":digest(&old),"evidenceDigest":stored.evidence_digest,"reviews":{},"scopeReview":[]});
        write_json(&root, &run, MAP_FILE, &legacy_map).unwrap();
        write_json(&root, &run, STATE_FILE, &legacy_state).unwrap();
        let map = model::parse_stored(&serde_json::to_vec(&legacy_map).unwrap()).unwrap();
        let map_bytes = fs::read(directory.join(MAP_FILE)).unwrap();
        let state_bytes = fs::read(directory.join(STATE_FILE)).unwrap();

        fs::write(root.join("test.js"), format!("\n{source}")).unwrap();
        assert!(load_inputs(&root, &run).is_err());
        let (imported, state) = load(&run, &stored).unwrap();
        let new =
            crate::assertion_inputs::capture(&root, "javascript", ["test.js".into()]).unwrap();
        let (next, state) =
            carry(&imported, &state, &stored.manifest, &new, "new-run", false).unwrap();
        assert_eq!(next.assertions[0].id, map.assertions[0].id);
        assert_eq!(
            next.assertions[0].flows[0].explanation,
            map.assertions[0].flows[0].explanation
        );
        assert!(next.assertions[0].flows[0].basis.is_none());
        assert!(!next.assertions[0].flows[0].questions.is_empty());
        assert_eq!(fs::read(directory.join(MAP_FILE)).unwrap(), map_bytes);
        assert_eq!(fs::read(directory.join(STATE_FILE)).unwrap(), state_bytes);

        // A checkout edited during a run must not prevent publication or
        // discard explanations. The run manifest still identifies its sites.
        let next_directory = crate::run_store::create_analyzable_test_run(&root, "next");
        let mut entries = read_archive(&path).unwrap();
        entries
            .iter_mut()
            .find(|e| e.path == ARCHIVE_PATH)
            .unwrap()
            .contents = serde_json::to_vec(&old.manifest()).unwrap();
        let raw = write_archive(entries, &next_directory.join("evidence.raw.gz")).unwrap();
        let mut next_metadata = metadata.clone();
        next_metadata.id = "next".into();
        next_metadata.started_at = "next".into();
        next_metadata.raw_evidence.compressed_bytes = raw.compressed_bytes;
        next_metadata.raw_evidence.uncompressed_bytes = raw.uncompressed_bytes;
        fs::write(
            next_directory.join("run.json"),
            serde_json::to_vec(&next_metadata).unwrap(),
        )
        .unwrap();
        fs::remove_file(root.join("test.js")).unwrap();
        prepare_publication(&root, &next_directory, &next_metadata).unwrap();
        let next_run = discover_runs(&root)
            .unwrap()
            .runs
            .into_iter()
            .find(|r| r.id == "next")
            .unwrap();
        let next_manifest = load_manifest(&next_run).unwrap();
        let (pending, pending_state) = load(&next_run, &next_manifest).unwrap();
        assert_eq!(pending.assertions[0], map.assertions[0]);
        assert!(!pending_state.changes.is_empty());
        assert!(
            !pending_state.flows
                [&flow_key(&pending.assertions[0], &pending.assertions[0].flows[0])]
                .reasons
                .is_empty()
        );
        assert!(load_inputs(&root, &next_run).is_err());
        assert_eq!(fs::read(directory.join(MAP_FILE)).unwrap(), map_bytes);
        assert_eq!(fs::read(directory.join(STATE_FILE)).unwrap(), state_bytes);

        let mut corrupt = state.clone();
        corrupt.evidence_digest = "wrong".into();
        write_json(&root, &run, STATE_FILE, &corrupt).unwrap();
        assert!(load(&run, &stored).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}
