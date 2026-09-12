//! Bounded Rust runtime baseline for assertion analysis.
//!
//! A phase proves *execution during an assertion*, not dependence on its
//! operands. In particular, `{ work(); 42 }` can discard work's result. Keep
//! runtime links separate from the join's value observations until source
//! analysis establishes that connection. No mutation predictions or global
//! assertion percentage are produced here.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::{
    asserted_coverage::{Boundary, Facts, Site, TestFacts},
    coverage_analysis::PointKind,
    coverage_report::{CoverageManifest, RawTestResult},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssertionExecutionLink {
    pub point: String,
    pub attempt: String,
    pub phase: String,
    pub operation: String,
    pub assertion_source: Option<String>,
    pub statement: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustRuntimeAssertionAnalysis {
    /// A deliberately limited denominator: compiler function-entry points in
    /// the explicitly requested files, including uncalled/unmeasured points.
    /// These are NOT all effect/decision sites and not a codebase percentage.
    pub facts: Facts,
    pub execution_links: Vec<AssertionExecutionLink>,
    /// A phase was entered but no outcome was attributed to it. Transport
    /// health alone does not establish semantic completeness of phase binding.
    pub unsettled_assertions: Vec<String>,
    pub unmeasured: Vec<String>,
    pub limitations: Vec<String>,
}

/// Analyze already-frozen evidence. This adds no probes to the test runtime.
///
/// No attempt is merged with another (including retries), no timestamps are
/// used for ownership, and failed/flaky/setup results cannot lend witnesses.
/// Missing assertion linkage remains a limit: an earlier computed value may
/// still be asserted through a local, a helper, or a joined thread.
pub fn analyze_runtime_assertions(
    manifest: &CoverageManifest,
    results: &[RawTestResult],
    source_files: &BTreeSet<String>,
) -> Result<RustRuntimeAssertionAnalysis, String> {
    let mut sites = BTreeMap::new();
    for point in &manifest.points {
        if point.kind != PointKind::Function || !source_files.contains(&point.file) {
            continue;
        }
        let bounds = vec![Boundary {
            boundary: format!("execution:{}", point.id),
            facet: None,
            via: None,
        }];
        let site = Site {
            id: point.id.clone(),
            file: point.file.clone(),
            line: u32::try_from(point.line).map_err(|_| "source line exceeds u32")?,
            kind: "effect".into(),
            category: "function-execution".into(),
            classification: "review".into(),
            owner: point.id.clone(),
            method: None,
            direct_bounds: bounds.clone(),
            bounds,
            reached: Vec::new(),
            covered_by: Vec::new(),
            object_valued_return: false,
            unmodelled_shapes: vec![
                "Runtime execution does not establish which function values or effects an assertion reads".into(),
            ],
            decision: None,
            derive: Vec::new(),
        };
        if sites.insert(point.id.clone(), site).is_some() {
            return Err(format!("duplicate function point {}", point.id));
        }
    }
    let unmeasured = manifest
        .unmeasured
        .iter()
        .filter(|id| sites.contains_key(*id))
        .cloned()
        .collect();
    let mut tests = Vec::new();
    let mut execution_links = Vec::new();
    let mut unsettled_assertions = Vec::new();
    for (index, result) in results.iter().enumerate() {
        if result.status.as_deref() != Some("passed") || result.role != "test" || result.flaky {
            continue;
        }
        let attempt = format!(
            "rust-attempt:{index}:{}",
            result.test_id.as_deref().unwrap_or(&result.test)
        );
        let mut phases = BTreeMap::new();
        for phase in &result.phases {
            if phases.insert(phase.id.as_str(), phase).is_some() {
                return Err(format!("duplicate phase {} in {attempt}", phase.id));
            }
            if phase.kind == "assertion" && phase.status.is_none() {
                unsettled_assertions.push(format!("{attempt}: {} ({})", phase.operation, phase.id));
            }
        }
        let mut covered = BTreeSet::new();
        let mut linked = BTreeSet::new();
        for snapshot in &result.runtime {
            covered.extend(
                snapshot
                    .hits
                    .iter()
                    .filter(|id| sites.contains_key(*id))
                    .cloned(),
            );
            for event in &snapshot.events {
                if event.environment != "rust"
                    || event.event_type != "hit"
                    || !sites.contains_key(&event.id)
                {
                    continue;
                }
                covered.insert(event.id.clone());
                let Some(phase_id) = &event.phase_id else {
                    continue;
                };
                let phase = phases
                    .get(phase_id.as_str())
                    .ok_or_else(|| format!("unknown phase {phase_id} in {attempt}"))?;
                if phase.kind != "assertion" || phase.status.as_deref() != Some("passed") {
                    continue;
                }
                if linked.insert((
                    event.id.clone(),
                    phase_id.clone(),
                    event.statement_id.clone(),
                )) {
                    execution_links.push(AssertionExecutionLink {
                        point: event.id.clone(),
                        attempt: attempt.clone(),
                        phase: phase_id.clone(),
                        operation: phase.operation.clone(),
                        assertion_source: phase.source.clone(),
                        statement: event.statement_id.clone(),
                    });
                }
            }
        }
        for id in covered {
            sites
                .get_mut(&id)
                .expect("filtered to known sites")
                .covered_by
                .push(attempt.clone());
        }
        tests.push(TestFacts {
            id: attempt,
            file: result.test_file.clone().unwrap_or_default(),
            // Do NOT turn temporal linkage into a value observation. The
            // existing join reports these covered sites as analysis limits.
            observations: Vec::new(),
            sinks: Vec::new(),
            rendered: Vec::new(),
            witness_issues: vec![],
        });
    }
    Ok(RustRuntimeAssertionAnalysis {
        facts: Facts {
            schema: 1,
            sites: sites.into_values().collect(),
            tests,
            mocks_by_test_file: BTreeMap::new(),
        },
        execution_links,
        unsettled_assertions,
        unmeasured,
        limitations: vec![
            "Function-entry baseline only; effect and decision site discovery is not implemented".into(),
            "Assertions do not expose data dependence, discarded values, masking, or custom equality".into(),
            "Precomputed operands and cross-thread/process value transfer need additional evidence".into(),
            "No assertion score or mutant-kill prediction is justified by these links".into(),
        ],
    })
}
