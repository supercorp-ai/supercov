//! Run-owned assertion map lifecycle and execution-backed, agent-assessed score.
use crate::{
    assertion_inputs::ARCHIVE_PATH,
    assertion_map::{self as model, *},
    coverage_report::{
        ArchiveReportRequest, CoverageReport, ExitCodeInput, analyze_coverage_archive,
    },
    evidence_archive::read_archive,
    lifecycle::atomic_write,
    run_store::StoredRun,
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
pub struct RunInputs {
    pub inputs: Inputs,
    pub evidence_digest: String,
}
pub fn load_inputs(run: &StoredRun) -> Result<RunInputs, String> {
    if run.metadata.merged == Some(true) {
        return Err(
            "Use a single run for assertion maps; merged runs have multiple input snapshots".into(),
        );
    }
    let bytes = fs::read(&run.evidence_path).map_err(|e| e.to_string())?;
    let evidence_digest = format!("{:x}", Sha256::digest(&bytes));
    let entries = read_archive(&run.evidence_path).map_err(|e| e.to_string())?;
    let input = entries.iter().find(|e| e.path == ARCHIVE_PATH).ok_or("This older run has no frozen assertion inputs. Run tests once with this version of Supercov")?;
    let inputs: Inputs = serde_json::from_slice(&input.contents).map_err(|e| e.to_string())?;
    if inputs.schema_version != 1 {
        return Err("Unsupported assertion input schema".into());
    }
    if inputs.files.keys().any(|f| !local_path(f))
        || inputs
            .assertions
            .iter()
            .any(|s| s.at.offset(&inputs.files).is_none())
    {
        return Err("Invalid frozen assertion inputs".into());
    }
    if fs::read(&run.evidence_path).map_err(|e| e.to_string())? != bytes {
        return Err("Run archive changed during read".into());
    }
    Ok(RunInputs {
        inputs,
        evidence_digest,
    })
}
pub fn load(run: &StoredRun, input: &RunInputs) -> Result<(AssertionMap, State), String> {
    let read = |file: &str| {
        fs::read(run.directory.join(file))
            .map_err(|e| format!("{file}: {e}; use assertions init first"))
    };
    let map = model::parse(&read(MAP_FILE)?).map_err(|e| format!("{MAP_FILE}: {e}"))?;
    let state: State =
        serde_json::from_slice(&read(STATE_FILE)?).map_err(|e| format!("{STATE_FILE}: {e}"))?;
    if state.schema_version != 1
        || state.inputs_digest != digest(&input.inputs)
        || state.evidence_digest != input.evidence_digest
    {
        return Err(
            "Assertion state belongs to different run evidence; initialize or carry into this run"
                .into(),
        );
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
/// Creation never overwrites authored work. A map without state after a failed
/// second write is recoverable via init, which retains the existing JSON.
pub fn initialize(
    root: &Path,
    run: &StoredRun,
    previous: Option<&StoredRun>,
) -> Result<Value, String> {
    let input = load_inputs(run)?;
    if run.directory.join(STATE_FILE).exists() {
        return Err("This run already has assertion state; edit its map and use review".into());
    }
    let (map, state) = if let Some(old_run) = previous {
        if old_run.id == run.id {
            return Err("Cannot inherit from the same run".into());
        }
        if run.directory.join(MAP_FILE).exists() {
            return Err("Refusing to replace an existing assertion map".into());
        }
        let old = load_inputs(old_run)?;
        let (map, state) = load(old_run, &old)?;
        let a = &old_run.metadata.integrity.fingerprint;
        let b = &run.metadata.integrity.fingerprint;
        // Execution fingerprint also contains source hashes. Compare context
        // components separately so source edits only dirty affected watches.
        let context_changed = a.configuration != b.configuration
            || a.dependencies != b.dependencies
            || a.instrumenter != b.instrumenter
            || old_run.metadata.command != run.metadata.command
            || old.inputs.language != input.inputs.language
            || old.inputs.context_digest != input.inputs.context_digest;
        model::carry(
            &map,
            &state,
            &old.inputs,
            &input.inputs,
            &input.evidence_digest,
            context_changed,
        )?
    } else if run.directory.join(MAP_FILE).exists() {
        let map = model::parse(&fs::read(run.directory.join(MAP_FILE)).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        (map, seed(&input.inputs, &input.evidence_digest).1)
    } else {
        seed(&input.inputs, &input.evidence_digest)
    };
    if !run.directory.join(MAP_FILE).exists() {
        write_json(root, run, MAP_FILE, &map)?;
    }
    write_json(root, run, STATE_FILE, &state)?;
    Ok(
        json!({"map":run.directory.join(MAP_FILE),"state":run.directory.join(STATE_FILE),"assertions":map.assertions.len(),"retiredAssertions":map.retired_assertions.len(),"scopeReview":state.scope_review}),
    )
}
pub fn acknowledge(
    root: &Path,
    run: &StoredRun,
    selected: &BTreeSet<String>,
    all: bool,
    ack_scope: bool,
) -> Result<Value, String> {
    let input = load_inputs(run)?;
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
fn phase_matches(
    a: &Assertion,
    phase: &crate::coverage_report::CoveragePhase,
    inputs: &Inputs,
) -> bool {
    if phase.kind != "assertion" || phase.status.as_deref() != Some("passed") {
        return false;
    }
    // A source coordinate alone is not an assertion identity: an agent could
    // accidentally anchor a larger enclosing expression at the same position.
    // Require the inventoried expression and operation as well for JS/TS.
    if inputs.language == "javascript"
        && !inputs
            .assertions
            .iter()
            .any(|s| s.at == a.at && s.operation == phase.operation)
    {
        return false;
    }
    let source = phase
        .operation
        .strip_prefix("Rust assertion at ")
        .or(phase.source.as_deref());
    let Some(location) = source else {
        return false;
    };
    let mut parts = location.rsplitn(3, ':');
    let Some(column) = parts.next().and_then(|n| n.parse::<usize>().ok()) else {
        return false;
    };
    let Some(line) = parts.next().and_then(|n| n.parse::<usize>().ok()) else {
        return false;
    };
    let Some(file) = parts.next() else {
        return false;
    };
    file == a.at.file
        && line == a.at.line
        && inputs
            .files
            .get(file)
            .and_then(|text| byte_column(text, line, column, &inputs.language))
            == Some(a.at.column)
}

/// Recompute from the mutable map on every query; never cache it into the
/// immutable structural coverage index. Reports stay about the archived run.
pub fn report(run: &StoredRun) -> Result<Value, String> {
    let input = load_inputs(run)?;
    let (map, state) = load(run, &input)?;
    let coverage = coverage(run)?;
    let mut report = assess(
        &map,
        &state,
        &input.inputs,
        &coverage,
        run.metadata.test_exit_code == Some(0),
    );
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
    let identities_valid = state.inputs_digest == digest(inputs)
        && errors.iter().all(|e| {
            !e.contains("duplicate") && !e.contains("schema") && !e.contains("assertion ID")
        });
    for a in &map.assertions {
        let witnesses = view
            .phases
            .iter()
            .filter(|p| phase_matches(a, &p.phase, inputs))
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
        rows.push(json!({"id":a.id,"at":a.at,"analysis":a.analysis,"observedPassingTests":witnesses,"flows":flows}));
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
