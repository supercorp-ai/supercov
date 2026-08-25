//! Agent-facing queries over persisted run metadata.

use serde::Serialize;
use supercov_contracts::AgentPagination;

use crate::{
    agent_json,
    coverage_analysis::serialize_javascript_number,
    coverage_index::{CoverageIndex, CoverageViewId},
    coverage_query::CoverageQueryFilters,
    run_store::{
        RawEvidenceMetadata, RunIntegrity, RunInventory, RunTimings, compare_run_integrity,
        open_existing_query_index,
    },
};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunListEntry {
    pub id: String,
    pub generated_at: String,
    pub coverage_indexed: bool,
    #[serde(serialize_with = "serialize_optional_javascript_number")]
    pub lines: Option<f64>,
    #[serde(serialize_with = "serialize_optional_javascript_number")]
    pub branches: Option<f64>,
    #[serde(serialize_with = "serialize_optional_javascript_number")]
    pub mcdc: Option<f64>,
    pub command: Vec<String>,
    #[serde(serialize_with = "serialize_javascript_number")]
    pub duration_ms: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timings: Option<RunTimings>,
    pub test_exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build_reused: Option<bool>,
    pub raw_evidence: RawEvidenceMetadata,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    pub reasons: Vec<String>,
}

fn serialize_optional_javascript_number<S>(
    value: &Option<f64>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    match value {
        Some(value) => serialize_javascript_number(value, serializer),
        None => serializer.serialize_none(),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunListData {
    pub filters: CoverageQueryFilters,
    pub runs: Vec<RunListEntry>,
}

pub fn run_list_query(
    inventory: &RunInventory,
    current_integrity: Option<&RunIntegrity>,
    view: CoverageViewId,
    offset: usize,
    limit: usize,
) -> (RunListData, AgentPagination) {
    let runs = inventory
        .runs
        .iter()
        .skip(offset)
        .take(limit)
        .map(|run| {
            let summary = open_existing_query_index(run)
                .ok()
                .flatten()
                .and_then(|container| CoverageIndex::new(&container).ok()?.summary(view).ok());
            let comparison = current_integrity
                .map(|current| compare_run_integrity(Some(&run.metadata.integrity), current));
            RunListEntry {
                id: run.id.clone(),
                generated_at: run.metadata.started_at.clone(),
                coverage_indexed: summary.is_some(),
                lines: summary.as_ref().map(|summary| summary.lines.percentage),
                branches: summary.as_ref().map(|summary| summary.branches.percentage),
                mcdc: summary
                    .as_ref()
                    .map(|summary| summary.condition_coverage_pct),
                command: run.metadata.command.clone(),
                duration_ms: run.metadata.duration_ms,
                timings: run.metadata.timings.clone(),
                test_exit_code: run.metadata.test_exit_code,
                build_reused: run
                    .metadata
                    .instrumented_build_cache
                    .as_ref()
                    .map(|cache| cache.reused),
                raw_evidence: run.metadata.raw_evidence.clone(),
                stale: comparison.as_ref().map(|comparison| comparison.stale),
                reasons: comparison
                    .map(|comparison| comparison.reasons)
                    .unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    let page = agent_json::pagination(offset, limit, runs.len(), inventory.runs.len());
    (
        RunListData {
            filters: CoverageQueryFilters {
                outcome: match view {
                    CoverageViewId::All => "all",
                    CoverageViewId::Passed => "passed",
                    CoverageViewId::Failed => "failed",
                }
                .into(),
                kind: None,
                runner: None,
            },
            runs,
        },
        page,
    )
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::{Path, PathBuf},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::run_store::{discover_runs, open_or_rebuild_query_index, select_run};

    use super::*;

    fn temporary_directory() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("supercov-run-query-{}-{nonce}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn copy_real_fixture_run(root: &Path) -> RunInventory {
        let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let fixture = workspace.join("tests/fixtures/generic-webpack");
        let source_inventory = discover_runs(&fixture).unwrap();
        let source = select_run(&source_inventory, Some("latest")).unwrap();
        let destination = root.join(".supercov/runs").join(&source.id);
        fs::create_dir_all(&destination).unwrap();
        fs::copy(&source.metadata_path, destination.join("run.json")).unwrap();
        fs::copy(&source.evidence_path, destination.join("evidence.raw.gz")).unwrap();
        discover_runs(root).unwrap()
    }

    #[test]
    fn lists_persisted_metadata_without_building_an_index_then_reads_the_typed_index() {
        let root = temporary_directory();
        let inventory = copy_real_fixture_run(&root);
        let run = &inventory.runs[0];
        let (before, page) = run_list_query(&inventory, None, CoverageViewId::All, 0, 20);
        assert_eq!(page.total, 1);
        assert!(!before.runs[0].coverage_indexed);
        assert_eq!(before.runs[0].lines, None);
        assert!(!run.query_index_path.exists());

        open_or_rebuild_query_index(run).unwrap();
        let (after, page) = run_list_query(
            &inventory,
            Some(&run.metadata.integrity),
            CoverageViewId::Passed,
            0,
            20,
        );
        assert!(after.runs[0].coverage_indexed);
        assert_eq!(after.runs[0].lines, Some(100.0));
        assert_eq!(after.runs[0].branches, Some(100.0));
        assert_eq!(after.runs[0].mcdc, Some(100.0));
        assert_eq!(after.runs[0].stale, Some(false));
        assert!(after.runs[0].reasons.is_empty());
        assert_eq!(after.filters.outcome, "passed");
        assert!(agent_json::success("runs", &after, Some(&page)).is_ok());

        let (_, empty_page) = run_list_query(&inventory, None, CoverageViewId::All, 20, 20);
        assert_eq!(empty_page.returned, 0);
        assert!(!empty_page.has_more);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reports_staleness_in_contract_order_and_treats_a_bad_index_as_disposable() {
        let root = temporary_directory();
        let inventory = copy_real_fixture_run(&root);
        let run = &inventory.runs[0];
        open_or_rebuild_query_index(run).unwrap();
        fs::write(&run.query_index_path, b"broken disposable index").unwrap();

        let mut current = run.metadata.integrity.clone();
        current.fingerprint.source = "1".repeat(64);
        current.fingerprint.tests = "2".repeat(64);
        let (listing, _) = run_list_query(&inventory, Some(&current), CoverageViewId::All, 0, 20);
        assert!(!listing.runs[0].coverage_indexed);
        assert_eq!(
            listing.runs[0].reasons,
            ["instrumented source changed", "test files changed"]
        );
        assert_eq!(
            fs::read(&run.query_index_path).unwrap(),
            b"broken disposable index"
        );
        fs::remove_dir_all(root).unwrap();
    }
}
