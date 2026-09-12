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
pub struct RunManifest {
    pub manifest: InputManifest,
    pub evidence_digest: String,
    legacy_digest: Option<String>,
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
    }))
}
pub fn load(run: &StoredRun, input: &RunManifest) -> Result<(AssertionMap, State), String> {
    let read = |file: &str| {
        fs::read(run.directory.join(file))
            .map_err(|e| format!("{file}: {e}; new test runs create assertion maps automatically"))
    };
    let map = model::parse(&read(MAP_FILE)?).map_err(|e| format!("{MAP_FILE}: {e}"))?;
    let mut state: State =
        serde_json::from_slice(&read(STATE_FILE)?).map_err(|e| format!("{STATE_FILE}: {e}"))?;
    let identity = digest(&input.manifest);
    let legacy =
        state.schema_version == 1 && input.legacy_digest.as_ref() == Some(&state.inputs_digest);
    if state.evidence_digest != input.evidence_digest
        || !(legacy || state.schema_version == 2 && state.inputs_digest == identity)
    {
        return Err(
            "Assertion state belongs to different run evidence; rerun tests to create a bound map"
                .into(),
        );
    }
    if legacy {
        state.schema_version = 2;
        state.inputs_digest = identity;
        for review in state.reviews.values_mut() {
            review.reasons.insert(
                "Imported legacy map; review against current source and file dependencies".into(),
            );
        }
    }
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
                            for flow in &assertion.flows {
                                next_state.reviews.insert(
                                    flow_key(&assertion, flow),
                                    Review {
                                        fingerprint: String::new(),
                                        reasons: BTreeSet::from([reason.clone()]),
                                    },
                                );
                            }
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
                    next_state.scope_review.insert(reason.clone());
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
        for review in state.reviews.values_mut() {
            review
                .reasons
                .insert("newer assertion map could not be reused; review inherited claims".into());
        }
    }
    if let Err(reason) = current {
        state.scope_review.insert(reason);
    }
    state.inheritance = Some(inheritance);
    write_json(root, &run, MAP_FILE, &map)?;
    write_json(root, &run, STATE_FILE, &state)?;
    Ok(())
}
pub fn acknowledge(
    root: &Path,
    run: &StoredRun,
    selected: &BTreeSet<String>,
    all: bool,
    ack_scope: bool,
) -> Result<Value, String> {
    let input = load_inputs(root, run)?;
    let (map, mut state) = load(run, &input)?;
    model::review(&map, &mut state, &input.inputs, selected, all, ack_scope)?;
    write_json(root, run, STATE_FILE, &state)?;
    Ok(
        json!({"reviewedFlows":if all { map.assertions.iter().map(|a| a.flows.len()).sum::<usize>() } else { selected.len() }, "scopeReview":state.scope_review,"meaning":"Agent acknowledgement recorded; semantic edges were not mechanically proved"}),
    )
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

/// Recompute from the mutable map on every query; never cache it into the
/// immutable structural coverage index. Current source must match the run.
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
    let coverage = coverage(run)?;
    let mut report = assess(
        &map,
        &state,
        &input.inputs,
        &coverage,
        run.metadata.test_exit_code == Some(0),
    );
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

pub fn assess(
    map: &AssertionMap,
    state: &State,
    inputs: &Inputs,
    coverage: &CoverageReport,
    passed: bool,
) -> Value {
    let errors = model::validate(map, inputs);
    let view = &coverage.filters.passed;
    let tests = view
        .tests
        .iter()
        .map(|t| (&t.id, t))
        .collect::<BTreeMap<_, _>>();
    let measured_statements = view
        .points
        .iter()
        .filter(|p| p.measured && p.meta.kind == crate::coverage_analysis::PointKind::Statement)
        .collect::<Vec<_>>();
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
    let mut dirty_flows = 0;
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
            let dirty = model::reasons(a, f, state, inputs);
            if dirty.is_empty() {
                current_flows += 1;
            } else {
                dirty_flows += 1;
            }
            let applicable = witnesses
                .iter()
                .filter(|id| {
                    tests.get(id).is_some_and(|test| {
                        f.applies_to.is_empty() || f.applies_to.contains(&test.name)
                    })
                })
                .cloned()
                .collect::<BTreeSet<_>>();
            let eligible = passed
                && identities_valid
                && state.scope_review.is_empty()
                && dirty.is_empty()
                && !applicable.is_empty()
                && a.analysis != Analysis::Unmapped;
            let mut blockers = Vec::new();
            if !passed {
                blockers.push("run did not pass");
            }
            if !identities_valid {
                blockers.push("invalid map identities");
            }
            if !state.scope_review.is_empty() {
                blockers.push("scope review pending");
            }
            if !dirty.is_empty() {
                blockers.push("flow requires review or reference repair");
            }
            if applicable.is_empty() {
                blockers.push("no matching passing assertion occurrence for appliesTo");
            }
            if a.analysis == Analysis::Unmapped {
                blockers.push("assertion is unmapped");
            }
            if eligible {
                credit_flows += 1;
            }
            let mut lines = BTreeSet::new();
            for node in f
                .nodes
                .iter()
                .filter(|n| f.counts_as_asserted.contains(&n.id))
            {
                let Some(start) = node.at.offset(&inputs.files) else {
                    continue;
                };
                let Some(points) = by_file.get(node.at.file.as_str()) else {
                    continue;
                };
                let first = points.partition_point(|(pos, _)| *pos < start);
                for (pos, point) in points[first..].iter().take_while(|(pos, _)| *pos == start) {
                    // One explicit node credits one exact measured statement.
                    // A guard/class/block claim must not silently credit every
                    // nested statement just because its source span contains it.
                    if *pos != start || point.meta.source != node.at.text {
                        continue;
                    }
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
            flows.push(json!({"id":f.id,"current":dirty.is_empty(),"reasons":dirty,"eligible":eligible,"blockers":blockers,"matchingTests":applicable,"creditedStatementLines":lines}));
        }
        rows.push(json!({"id":a.id,"at":a.at,"analysis":a.analysis,"inMap":in_map,"observes":a.observes,"operations":inventory.get(&a.at).cloned().unwrap_or_default(),"observedPassingTests":witnesses,"flows":flows}));
    }
    let denominator = coverage
        .view
        .lines
        .iter()
        .filter(|l| l.measured)
        .map(|l| (l.file.clone(), l.line))
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
    let mapping_complete = missing_inventory == 0
        && map
            .assertions
            .iter()
            .all(|a| a.analysis == Analysis::Mapped && !a.flows.is_empty())
        && dirty_flows == 0
        && state.scope_review.is_empty()
        && errors.is_empty();
    let total = denominator.len();
    let statements = measured_statements.iter().map(|p| {
        let at = inputs.files.get(&p.meta.file)
            .and_then(|text| byte_column(text, p.meta.line, p.meta.column, &inputs.language))
            .map(|column| Anchor { file: p.meta.file.clone(), line: p.meta.line, column, text: p.meta.source.clone() })
            .filter(|at| at.offset(&inputs.files).is_some());
        json!({"id":p.meta.id,"file":p.meta.file,"line":p.meta.line,"at":at,"covered":p.covered,"tests":p.tests,"declared":claimed_points.contains(&p.meta.id),"asserted":credited_points.contains(&p.meta.id),"flows":point_flows.get(&p.meta.id).cloned().unwrap_or_default()})
    }).collect::<Vec<_>>();
    json!({"basis":"agent-assessed; passing assertion identity and same-test execution required; not mutation resistance",
        "summary":{"metric":"measured statements","statements":{"asserted":credited_points.len(),"declared":claimed_points.len(),"total":measured_statements.len(),"percentage":if measured_statements.is_empty() { None } else {Some(credited_points.len() as f64 * 100.0 / measured_statements.len() as f64)}},"assertions":map.assertions.len(),"inventoryAssertions":inputs.assertions.len(),"missingInventoryAssertions":missing_inventory,
            "unmappedAssertions":map.assertions.iter().filter(|a| a.analysis==Analysis::Unmapped || a.flows.is_empty()).count(),
            "currentFlows":current_flows,"dirtyFlows":dirty_flows,"eligibleFlows":credit_flows,"retiredAssertions":map.retired_assertions.len(),
            "unobservedAssertions":rows.iter().filter(|a| a["observedPassingTests"].as_array().is_none_or(Vec::is_empty)).count(),
            "inventoryFailures":inputs.limitations.iter().filter(|s| s.starts_with("Inventory unavailable for ")).count(),
            "unanchoredStatements":statements.iter().filter(|s| s["at"].is_null()).count(),
            "inventoryMappingComplete":mapping_complete,"runPassed":passed,"scopeReview":state.scope_review,
            "lines":{"asserted":credited.len(),"declared":declared.len(),"total":total,"percentage":if total==0 {None} else {Some(credited.len() as f64 * 100.0 / total as f64)}}},
        "assertions":rows,"statements":statements,"tests":tests.values().map(|t| json!({"id":t.id,"name":t.name})).collect::<Vec<_>>(),
        "creditedLines":credited.iter().map(|loc| json!({"file":loc.0,"line":loc.1,"assertions":line_assertions.get(loc)})).collect::<Vec<_>>(),
        "unassertedLines":denominator.difference(&credited).map(|(f,l)| json!({"file":f,"line":l})).collect::<Vec<_>>(),
        "validationErrors":errors,"limitations":inputs.limitations})
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let (mut map, mut state) = seed(&old, &stored.evidence_digest);
        map.assertions[0].analysis = Analysis::Mapped;
        map.assertions[0].flows.push(Flow {
            id: "constant".into(),
            explanation: "The assertion checks the constant one.".into(),
            applies_to: vec![],
            nodes: vec![],
            edges: vec![],
            counts_as_asserted: vec![],
            watch: vec![Watch::File {
                file: "test.js".into(),
            }],
        });
        review(&map, &mut state, &old, &BTreeSet::new(), true, false).unwrap();
        state.schema_version = 1;
        state.inputs_digest = digest(&old);
        write_json(&root, &run, MAP_FILE, &map).unwrap();
        write_json(&root, &run, STATE_FILE, &state).unwrap();
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
        assert!(
            state
                .reviews
                .values()
                .all(|r| r.reasons.iter().any(|r| r.contains("legacy")))
        );
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
        assert!(!pending_state.scope_review.is_empty());
        assert!(
            !pending_state.reviews
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
