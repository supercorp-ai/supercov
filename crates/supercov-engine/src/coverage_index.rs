//! Typed coverage columns stored in the immutable query-index container.
//!
//! This is not a serialized report. Records contain fixed-width values and
//! checked references into an interned UTF-8 string table. New query surfaces
//! add sections without forcing existing readers to parse unrelated data.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::{
    coverage_analysis::{
        CoverageCount, CoverageSummary, McdcVector, find_witnesses_for_conditions,
    },
    coverage_report::{CoverageReport, CoverageView, TransportStats, coverage_summary_for_tests},
    query_index::{QueryIndex, QueryIndexError, QueryIndexSection},
};

pub const SECTION_STRING_BYTES: u32 = 1;
pub const SECTION_STRINGS: u32 = 2;
pub const SECTION_STRING_RELATIONS: u32 = 3;
pub const SECTION_VIEW_SUMMARIES: u32 = 10;
pub const SECTION_FILE_GAPS: u32 = 11;
pub const SECTION_DECISION_GAPS: u32 = 12;
pub const SECTION_DIMENSIONS: u32 = 13;
pub const SECTION_PROJECTIONS: u32 = 14;
pub const SECTION_SCOPE_ENTRIES: u32 = 15;
pub const SECTION_CONFIDENCE: u32 = 16;
pub const SECTION_LINES: u32 = 17;
pub const SECTION_TEST_SUMMARIES: u32 = 18;
pub const SECTION_PHASE_SUMMARIES: u32 = 19;
pub const SECTION_ANCHORS: u32 = 20;
pub const SECTION_TEST_RETRIES: u32 = 21;
pub const SECTION_TEST_ATTEMPTS: u32 = 22;
pub const SECTION_TEST_LINES: u32 = 23;
pub const SECTION_TEST_HITS: u32 = 24;
pub const SECTION_TEST_DECISIONS: u32 = 25;
pub const SECTION_TEST_VECTORS: u32 = 26;
pub const SECTION_VECTOR_VALUES: u32 = 27;
pub const SECTION_HIT_METADATA: u32 = 28;
pub const SECTION_DECISION_METADATA: u32 = 29;

const STRING_RECORD_SIZE: usize = 16;
const SUMMARY_RECORD_SIZE: usize = 176;
const FILE_GAP_RECORD_SIZE: usize = 176;
const DECISION_GAP_RECORD_SIZE: usize = 96;
const DIMENSION_RECORD_SIZE: usize = 192;
const PROJECTION_RECORD_SIZE: usize = 512;
const SCOPE_ENTRY_RECORD_SIZE: usize = 96;
const CONFIDENCE_RECORD_SIZE: usize = 96;
const LINE_RECORD_SIZE: usize = 80;
const TEST_SUMMARY_RECORD_SIZE: usize = 64;
const PHASE_SUMMARY_RECORD_SIZE: usize = 64;
const ANCHOR_RECORD_SIZE: usize = 64;
const TEST_RETRY_RECORD_SIZE: usize = 16;
const TEST_ATTEMPT_RECORD_SIZE: usize = 24;
const TEST_LINE_RECORD_SIZE: usize = 24;
const TEST_HIT_RECORD_SIZE: usize = 16;
const TEST_DECISION_RECORD_SIZE: usize = 32;
const TEST_VECTOR_RECORD_SIZE: usize = 24;
const HIT_METADATA_RECORD_SIZE: usize = 64;
const DECISION_METADATA_RECORD_SIZE: usize = 64;
const NO_STRING: u32 = u32::MAX;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CoverageViewId {
    All = 0,
    Passed = 1,
    Failed = 2,
}

impl TryFrom<u8> for CoverageViewId {
    type Error = CoverageIndexError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::All),
            1 => Ok(Self::Passed),
            2 => Ok(Self::Failed),
            _ => Err(CoverageIndexError::InvalidRecord("coverage view")),
        }
    }
}

#[derive(Debug)]
pub enum CoverageIndexError {
    Container(QueryIndexError),
    InvalidRecord(&'static str),
    InvalidUtf8,
    SizeOverflow,
}

impl From<QueryIndexError> for CoverageIndexError {
    fn from(value: QueryIndexError) -> Self {
        Self::Container(value)
    }
}

impl std::fmt::Display for CoverageIndexError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Container(error) => write!(formatter, "{error}"),
            Self::InvalidRecord(reason) => write!(formatter, "invalid coverage index: {reason}"),
            Self::InvalidUtf8 => write!(formatter, "invalid UTF-8 in coverage index"),
            Self::SizeOverflow => write!(formatter, "coverage index exceeds format limits"),
        }
    }
}

impl std::error::Error for CoverageIndexError {}

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(bytes: &[u8], offset: usize) -> Result<u32, CoverageIndexError> {
    Ok(u32::from_le_bytes(
        bytes
            .get(offset..offset + 4)
            .and_then(|value| value.try_into().ok())
            .ok_or(CoverageIndexError::InvalidRecord("truncated u32"))?,
    ))
}

fn get_u64(bytes: &[u8], offset: usize) -> Result<u64, CoverageIndexError> {
    Ok(u64::from_le_bytes(
        bytes
            .get(offset..offset + 8)
            .and_then(|value| value.try_into().ok())
            .ok_or(CoverageIndexError::InvalidRecord("truncated u64"))?,
    ))
}

fn usize_u64(value: usize) -> Result<u64, CoverageIndexError> {
    u64::try_from(value).map_err(|_| CoverageIndexError::SizeOverflow)
}

fn usize_u32(value: usize) -> Result<u32, CoverageIndexError> {
    u32::try_from(value).map_err(|_| CoverageIndexError::SizeOverflow)
}

#[derive(Default)]
struct StringTable {
    ids: HashMap<String, u32>,
    strings: Vec<String>,
}

#[derive(Default)]
struct StringRelations {
    values: Vec<u32>,
}

impl StringRelations {
    fn push(
        &mut self,
        values: impl IntoIterator<Item = String>,
        strings: &mut StringTable,
    ) -> Result<(u64, u64), CoverageIndexError> {
        let offset = usize_u64(self.values.len())?;
        for value in values {
            self.values.push(strings.intern(&value)?);
        }
        Ok((offset, usize_u64(self.values.len())? - offset))
    }

    fn section(self) -> Result<QueryIndexSection, CoverageIndexError> {
        let mut bytes = Vec::with_capacity(self.values.len() * 4);
        for value in self.values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        Ok(QueryIndexSection {
            kind: SECTION_STRING_RELATIONS,
            record_size: 4,
            count: usize_u64(bytes.len() / 4)?,
            bytes,
        })
    }
}

impl StringTable {
    fn intern(&mut self, value: &str) -> Result<u32, CoverageIndexError> {
        if let Some(id) = self.ids.get(value) {
            return Ok(*id);
        }
        let id = usize_u32(self.strings.len())?;
        self.ids.insert(value.into(), id);
        self.strings.push(value.into());
        Ok(id)
    }

    fn sections(self) -> Result<[QueryIndexSection; 2], CoverageIndexError> {
        let mut blob = Vec::new();
        let mut records = Vec::with_capacity(self.strings.len() * STRING_RECORD_SIZE);
        for string in self.strings {
            let offset = usize_u64(blob.len())?;
            let value = string.as_bytes();
            let length = usize_u32(value.len())?;
            blob.extend_from_slice(value);
            let mut record = [0_u8; STRING_RECORD_SIZE];
            put_u64(&mut record, 0, offset);
            put_u32(&mut record, 8, length);
            records.extend_from_slice(&record);
        }
        Ok([
            QueryIndexSection {
                kind: SECTION_STRING_BYTES,
                record_size: 0,
                count: usize_u64(blob.len())?,
                bytes: blob,
            },
            QueryIndexSection {
                kind: SECTION_STRINGS,
                record_size: STRING_RECORD_SIZE as u32,
                count: usize_u64(records.len() / STRING_RECORD_SIZE)?,
                bytes: records,
            },
        ])
    }
}

fn put_count(
    bytes: &mut [u8],
    offset: usize,
    count: &CoverageCount,
) -> Result<(), CoverageIndexError> {
    put_u64(bytes, offset, usize_u64(count.covered)?);
    put_u64(bytes, offset + 8, usize_u64(count.total)?);
    Ok(())
}

fn summary_record(
    id: CoverageViewId,
    view: &CoverageView,
    strings: &mut StringTable,
) -> Result<[u8; SUMMARY_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; SUMMARY_RECORD_SIZE];
    record[0] = id as u8;
    record[1] = u8::from(view.summary.coverage_complete);
    record[2] = match view.summary.completeness_blocked {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    };
    put_u32(&mut record, 4, strings.intern(&view.generated_at)?);
    put_u32(&mut record, 8, strings.intern(&view.variant)?);
    let values = [
        view.summary.decisions,
        view.summary.executed_decisions,
        view.summary.covered_decisions,
        view.summary.conditions,
        view.summary.covered_conditions,
    ];
    for (index, value) in values.into_iter().enumerate() {
        put_u64(&mut record, 16 + index * 8, usize_u64(value)?);
    }
    for (index, count) in [
        &view.summary.lines,
        &view.summary.statements,
        &view.summary.functions,
        &view.summary.branches,
        &view.summary.decision_outcomes,
        &view.summary.condition_outcomes,
        &view.summary.value_selections,
    ]
    .into_iter()
    .enumerate()
    {
        put_count(&mut record, 56 + index * 16, count)?;
    }
    Ok(record)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedFileGap {
    #[serde(skip)]
    pub view: CoverageViewId,
    pub file: String,
    pub uncovered_lines: usize,
    pub uncovered_statements: usize,
    pub uncovered_functions: usize,
    pub missing_branches: usize,
    pub missing_mcdc_conditions: usize,
    pub measurement_limitations: usize,
    pub limitation_kinds: Vec<String>,
    pub covered_by_other_tests: IndexedGapDimensions,
    pub uncovered_everywhere: IndexedGapDimensions,
    pub score: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedGapDimensions {
    pub lines: usize,
    pub statements: usize,
    pub functions: usize,
    pub branches: usize,
    pub mcdc_conditions: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedCoverageSnapshot {
    pub all_summary: CoverageSummary,
    pub passed_summary: CoverageSummary,
    pub failed_summary: CoverageSummary,
    pub all_files: Vec<IndexedFileGap>,
    pub passed_files: Vec<IndexedFileGap>,
    pub failed_files: Vec<IndexedFileGap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedDecisionGap {
    #[serde(skip)]
    pub view: CoverageViewId,
    #[serde(skip)]
    pub file: String,
    pub id: String,
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub conditions: usize,
    pub missing_conditions: usize,
    pub waived_conditions: usize,
    pub source: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageDimension {
    Kind = 0,
    Runner = 1,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedDimensionCoverage {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<String>,
    pub tests: usize,
    pub setups: usize,
    pub summary: CoverageSummary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedAttribution {
    pub browser_explicit: usize,
    pub browser_fallback: usize,
    pub server_explicit: usize,
    pub server_fallback: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedOutcomeCounts {
    pub passed: usize,
    pub failed: usize,
    pub flaky: usize,
    pub skipped: usize,
    pub timed_out: usize,
    pub interrupted: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedMeasurementKinds {
    #[serde(rename = "dynamic-code")]
    pub dynamic_code: usize,
    #[serde(rename = "semantic-safety")]
    pub semantic_safety: usize,
    #[serde(rename = "source-scope")]
    pub source_scope: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedMeasurement {
    pub complete: bool,
    pub limitations: usize,
    pub evidence_corruptions: usize,
    pub blocking: usize,
    pub files: usize,
    pub by_kind: IndexedMeasurementKinds,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedConfidenceLines {
    pub unexecuted: usize,
    pub executed: usize,
    pub action: usize,
    pub asserted: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSummaryConfidence {
    pub lines: IndexedConfidenceLines,
    pub assertion_covered_mcdc_conditions: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedSourceScope {
    pub mode: String,
    pub roots: Vec<String>,
    pub included: usize,
    pub excluded: usize,
    pub ambiguous: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedProjection {
    pub view: CoverageViewId,
    pub kind: Option<String>,
    pub runner: Option<String>,
    pub generated_at: String,
    pub summary: CoverageSummary,
    pub measurement: IndexedMeasurement,
    pub attribution: IndexedAttribution,
    pub transport: Option<TransportStats>,
    pub empty_evidence_tests: usize,
    pub first_empty_evidence_test: Option<String>,
    pub confidence: IndexedSummaryConfidence,
    pub files_with_gaps: usize,
    pub files_with_coverage_gaps: usize,
    pub tests: usize,
    pub setups: usize,
    pub test_outcomes: IndexedOutcomeCounts,
    pub source_scope: Option<IndexedSourceScope>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexedScopeEntry {
    pub file: String,
    pub status: String,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_root: Option<String>,
    pub measurement_limitations: usize,
    pub limitation_kinds: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedLine {
    pub file: String,
    pub line: usize,
    pub covered: bool,
    pub tests: Vec<String>,
    pub phases: Vec<String>,
    pub confidence: crate::coverage_report::CoverageConfidence,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedTestSummary {
    pub id: String,
    pub name: String,
    pub file: Option<String>,
    pub title: Option<String>,
    pub outcome: String,
    pub role: String,
    pub provenance: crate::coverage_report::TestProvenance,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedPhaseSummary {
    pub id: String,
    pub kind: String,
    pub operation: String,
    pub source: Option<String>,
    pub test: String,
    pub status: Option<String>,
    pub caused_by_phase_id: Option<String>,
    pub lines: usize,
    pub decisions: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedAnchor {
    pub kind: String,
    pub id: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub covered: bool,
    pub conditions: Option<usize>,
    pub covered_conditions: Option<usize>,
    pub tests: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct IndexedTestDetail {
    pub summary: IndexedTestSummary,
    pub retries: Vec<usize>,
    pub attempts: Vec<crate::coverage_report::TestAttempt>,
    pub hits: Vec<String>,
    pub decisions: Vec<crate::coverage_report::TestDecisionResult>,
    pub lines: Vec<crate::coverage_report::SourceLine>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedHitMetadata {
    pub id: String,
    pub obligation: String,
    pub branch_kind: Option<String>,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub label: Option<String>,
    pub alternative: Option<String>,
}

#[derive(Default)]
struct MutableFileGap {
    uncovered_lines: usize,
    uncovered_statements: usize,
    uncovered_functions: usize,
    missing_branches: usize,
    missing_mcdc_conditions: usize,
    measurement_limitations: usize,
    limitation_mask: u32,
    covered_by_other_tests: [usize; 5],
    uncovered_everywhere: [usize; 5],
}

fn limitation_kind(value: &serde_json::Value) -> Option<(&str, &str)> {
    Some((value.get("file")?.as_str()?, value.get("kind")?.as_str()?))
}

fn includes_selected(tests: &[String], selected: Option<&BTreeSet<String>>, covered: bool) -> bool {
    selected.map_or(covered, |selected| {
        tests.iter().any(|test| selected.contains(test))
    })
}

fn classify(
    gap: &mut MutableFileGap,
    dimension: usize,
    selected: Option<&BTreeSet<String>>,
    covered_overall: bool,
) {
    if selected.is_some() && covered_overall {
        gap.covered_by_other_tests[dimension] += 1;
    } else {
        gap.uncovered_everywhere[dimension] += 1;
    }
}

fn file_gaps(
    view: &CoverageView,
    selected: Option<&BTreeSet<String>>,
) -> Result<Vec<(String, MutableFileGap)>, CoverageIndexError> {
    let mut files = BTreeMap::<String, MutableFileGap>::new();
    for line in &view.lines {
        let gap = files.entry(line.file.clone()).or_default();
        if !includes_selected(&line.tests, selected, line.covered) {
            gap.uncovered_lines += 1;
            classify(gap, 0, selected, line.covered);
        }
    }
    for point in &view.points {
        let gap = files.entry(point.meta.file.clone()).or_default();
        if !includes_selected(&point.tests, selected, point.covered) {
            match point.meta.kind {
                crate::coverage_analysis::PointKind::Statement => {
                    gap.uncovered_statements += 1;
                    classify(gap, 1, selected, point.covered);
                }
                crate::coverage_analysis::PointKind::Function => {
                    gap.uncovered_functions += 1;
                    classify(gap, 2, selected, point.covered);
                }
            }
        }
    }
    for branch in &view.branches {
        let gap = files.entry(branch.meta.file.clone()).or_default();
        for alternative in &branch.alternatives {
            if !includes_selected(&alternative.tests, selected, alternative.covered) {
                gap.missing_branches += 1;
                classify(gap, 3, selected, alternative.covered);
            }
        }
    }
    for decision in &view.decisions {
        let gap = files.entry(decision.meta.file.clone()).or_default();
        let selected_vectors = decision
            .vector_observations
            .iter()
            .filter(|observation| includes_selected(&observation.tests, selected, true))
            .map(|observation| observation.vector.clone())
            .collect::<Vec<_>>();
        let witnesses =
            find_witnesses_for_conditions(&selected_vectors, decision.meta.conditions.len())
                .map_err(|_| CoverageIndexError::InvalidRecord("MC/DC vector width"))?;
        for (index, witness) in witnesses.into_iter().enumerate() {
            if witness.is_none() {
                gap.missing_mcdc_conditions += 1;
                classify(gap, 4, selected, decision.conditions[index].covered);
            }
        }
    }
    for limitation in &view.limitations {
        let Some((file, kind)) = limitation_kind(limitation) else {
            continue;
        };
        let gap = files.entry(file.into()).or_default();
        gap.measurement_limitations += 1;
        gap.limitation_mask |= match kind {
            "dynamic-code" => 1,
            "semantic-safety" => 2,
            "source-scope" => 4,
            _ => 8,
        };
    }
    Ok(files.into_iter().collect())
}

fn file_gap_record(
    view_id: CoverageViewId,
    file: &str,
    gap: &MutableFileGap,
    kind: Option<&str>,
    runner: Option<&str>,
    strings: &mut StringTable,
) -> Result<[u8; FILE_GAP_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; FILE_GAP_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(&mut record, 4, strings.intern(file)?);
    for (index, value) in [
        gap.uncovered_lines,
        gap.uncovered_statements,
        gap.uncovered_functions,
        gap.missing_branches,
        gap.missing_mcdc_conditions,
        gap.measurement_limitations,
    ]
    .into_iter()
    .enumerate()
    {
        put_u64(&mut record, 8 + index * 8, usize_u64(value)?);
    }
    put_u32(&mut record, 56, gap.limitation_mask);
    let score = gap.uncovered_lines
        + gap.uncovered_functions * 2
        + gap.missing_branches * 2
        + gap.missing_mcdc_conditions * 3
        + gap.measurement_limitations * 3;
    put_u64(&mut record, 64, usize_u64(score)?);
    put_u32(
        &mut record,
        72,
        kind.map_or(Ok(NO_STRING), |value| strings.intern(value))?,
    );
    put_u32(
        &mut record,
        76,
        runner.map_or(Ok(NO_STRING), |value| strings.intern(value))?,
    );
    for (index, value) in gap.covered_by_other_tests.into_iter().enumerate() {
        put_u64(&mut record, 80 + index * 8, usize_u64(value)?);
    }
    for (index, value) in gap.uncovered_everywhere.into_iter().enumerate() {
        put_u64(&mut record, 120 + index * 8, usize_u64(value)?);
    }
    Ok(record)
}

fn projections(view: &CoverageView) -> Vec<(Option<String>, Option<String>, BTreeSet<String>)> {
    let kinds = view
        .tests
        .iter()
        .map(|test| test.provenance.kind.clone())
        .collect::<BTreeSet<_>>();
    let runners = view
        .tests
        .iter()
        .map(|test| test.provenance.runner.clone())
        .collect::<BTreeSet<_>>();
    let mut selectors = Vec::new();
    for kind in &kinds {
        selectors.push((Some(kind.clone()), None));
    }
    for runner in &runners {
        selectors.push((None, Some(runner.clone())));
    }
    for kind in &kinds {
        for runner in &runners {
            selectors.push((Some(kind.clone()), Some(runner.clone())));
        }
    }
    selectors
        .into_iter()
        .filter_map(|(kind, runner)| {
            let selected = view
                .tests
                .iter()
                .filter(|test| {
                    kind.as_ref()
                        .is_none_or(|value| test.provenance.kind == *value)
                        && runner
                            .as_ref()
                            .is_none_or(|value| test.provenance.runner == *value)
                })
                .map(|test| test.id.clone())
                .collect::<BTreeSet<_>>();
            (!selected.is_empty()).then_some((kind, runner, selected))
        })
        .collect()
}

fn decision_gap_record(
    view_id: CoverageViewId,
    decision: &crate::coverage_report::DecisionResult,
    selected: Option<&BTreeSet<String>>,
    kind: Option<&str>,
    runner: Option<&str>,
    strings: &mut StringTable,
) -> Result<[u8; DECISION_GAP_RECORD_SIZE], CoverageIndexError> {
    let vectors = decision
        .vector_observations
        .iter()
        .filter(|observation| includes_selected(&observation.tests, selected, true))
        .map(|observation| observation.vector.clone())
        .collect::<Vec<_>>();
    let witnesses = find_witnesses_for_conditions(&vectors, decision.meta.conditions.len())
        .map_err(|_| CoverageIndexError::InvalidRecord("MC/DC vector width"))?;
    let mut record = [0_u8; DECISION_GAP_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(
        &mut record,
        4,
        kind.map_or(Ok(NO_STRING), |value| strings.intern(value))?,
    );
    put_u32(
        &mut record,
        8,
        runner.map_or(Ok(NO_STRING), |value| strings.intern(value))?,
    );
    put_u32(&mut record, 12, strings.intern(&decision.meta.id)?);
    put_u32(&mut record, 16, strings.intern(&decision.meta.file)?);
    put_u32(&mut record, 20, strings.intern(&decision.meta.kind)?);
    put_u32(
        &mut record,
        24,
        strings.intern(
            &decision
                .meta
                .source
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        )?,
    );
    put_u64(&mut record, 32, usize_u64(decision.meta.line)?);
    put_u64(&mut record, 40, usize_u64(decision.meta.column)?);
    put_u64(&mut record, 48, usize_u64(decision.meta.conditions.len())?);
    put_u64(
        &mut record,
        56,
        usize_u64(witnesses.iter().filter(|witness| witness.is_none()).count())?,
    );
    Ok(record)
}

fn put_summary_payload(
    record: &mut [u8],
    flags_offset: usize,
    base: usize,
    summary: &CoverageSummary,
) -> Result<(), CoverageIndexError> {
    record[flags_offset] = u8::from(summary.coverage_complete);
    record[flags_offset + 1] = match summary.completeness_blocked {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    };
    for (index, value) in [
        summary.decisions,
        summary.executed_decisions,
        summary.covered_decisions,
        summary.conditions,
        summary.covered_conditions,
    ]
    .into_iter()
    .enumerate()
    {
        put_u64(record, base + index * 8, usize_u64(value)?);
    }
    for (index, count) in [
        &summary.lines,
        &summary.statements,
        &summary.functions,
        &summary.branches,
        &summary.decision_outcomes,
        &summary.condition_outcomes,
        &summary.value_selections,
    ]
    .into_iter()
    .enumerate()
    {
        put_count(record, base + 40 + index * 16, count)?;
    }
    Ok(())
}

fn dimension_record(
    view_id: CoverageViewId,
    dimension: CoverageDimension,
    value: &crate::coverage_report::DimensionCoverage,
    strings: &mut StringTable,
) -> Result<[u8; DIMENSION_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; DIMENSION_RECORD_SIZE];
    record[0] = view_id as u8;
    record[1] = dimension as u8;
    let name = match dimension {
        CoverageDimension::Kind => value.kind.as_deref(),
        CoverageDimension::Runner => value.runner.as_deref(),
    }
    .ok_or(CoverageIndexError::InvalidRecord("dimension name"))?;
    put_u32(&mut record, 4, strings.intern(name)?);
    put_u64(&mut record, 8, usize_u64(value.tests)?);
    put_u64(&mut record, 16, usize_u64(value.setups)?);
    put_summary_payload(&mut record, 24, 32, &value.summary)?;
    Ok(record)
}

fn projection_record(
    view_id: CoverageViewId,
    view: &CoverageView,
    selected: Option<&BTreeSet<String>>,
    kind: Option<&str>,
    runner: Option<&str>,
    strings: &mut StringTable,
    relations: &mut StringRelations,
) -> Result<[u8; PROJECTION_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; PROJECTION_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(
        &mut record,
        4,
        kind.map_or(Ok(NO_STRING), |value| strings.intern(value))?,
    );
    put_u32(
        &mut record,
        8,
        runner.map_or(Ok(NO_STRING), |value| strings.intern(value))?,
    );
    put_u32(&mut record, 12, strings.intern(&view.generated_at)?);

    let scope = view.scope.as_ref();
    let scope_mode = scope
        .map(|value| {
            value
                .get("mode")
                .and_then(serde_json::Value::as_str)
                .ok_or(CoverageIndexError::InvalidRecord("source-scope mode"))
        })
        .transpose()?;
    record[2] = u8::from(scope_mode.is_some());
    put_u32(
        &mut record,
        16,
        scope_mode.map_or(Ok(NO_STRING), |value| strings.intern(value))?,
    );
    let roots = scope
        .map(|value| {
            value
                .get("roots")
                .and_then(serde_json::Value::as_array)
                .ok_or(CoverageIndexError::InvalidRecord("source-scope roots"))?
                .iter()
                .map(|root| {
                    root.as_str()
                        .map(str::to_owned)
                        .ok_or(CoverageIndexError::InvalidRecord("source-scope root"))
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?
        .unwrap_or_default();
    let (roots_offset, roots_count) = relations.push(roots, strings)?;
    put_u64(&mut record, 24, roots_offset);
    put_u32(
        &mut record,
        32,
        u32::try_from(roots_count).map_err(|_| CoverageIndexError::SizeOverflow)?,
    );

    let summary = selected.map_or_else(
        || Ok(view.summary.clone()),
        |ids| {
            coverage_summary_for_tests(view, ids)
                .map_err(|_| CoverageIndexError::InvalidRecord("projection summary"))
        },
    )?;
    put_summary_payload(&mut record, 36, 40, &summary)?;

    let mut limitation_kinds = [0_usize; 3];
    let mut limitation_files = BTreeSet::new();
    for limitation in &view.limitations {
        if let Some((file, kind)) = limitation_kind(limitation) {
            limitation_files.insert(file);
            match kind {
                "dynamic-code" => limitation_kinds[0] += 1,
                "semantic-safety" => limitation_kinds[1] += 1,
                "source-scope" => limitation_kinds[2] += 1,
                _ => {}
            }
        }
    }
    let corrupt_records = view
        .transport
        .as_ref()
        .map_or(0, |value| value.corrupt_records);
    let corrupt_files = view
        .transport
        .as_ref()
        .map_or(0, |value| value.corrupt_files);
    for (offset, value) in [
        (192, view.limitations.len()),
        (200, corrupt_records),
        (208, view.limitations.len() + corrupt_records),
        (216, limitation_files.len() + corrupt_files),
        (224, limitation_kinds[0]),
        (232, limitation_kinds[1]),
        (240, limitation_kinds[2]),
    ] {
        put_u64(&mut record, offset, usize_u64(value)?);
    }

    let phases = view
        .phases
        .iter()
        .filter(|phase| selected.is_none_or(|selected| selected.contains(&phase.test)));
    let mut attribution = [0_usize; 4];
    for phase in phases {
        attribution[0] += phase.explicit_browser_events;
        attribution[1] += phase.inferred_browser_events;
        attribution[2] += phase.explicit_server_events;
        attribution[3] += phase.inferred_server_events;
    }
    for (index, value) in attribution.into_iter().enumerate() {
        put_u64(&mut record, 248 + index * 8, usize_u64(value)?);
    }

    let confidence_levels = ["unexecuted", "executed", "action", "asserted"];
    for (index, level) in confidence_levels.into_iter().enumerate() {
        put_u64(
            &mut record,
            280 + index * 8,
            usize_u64(
                view.lines
                    .iter()
                    .filter(|line| line.confidence.level == level)
                    .count(),
            )?,
        );
    }
    put_u64(
        &mut record,
        312,
        usize_u64(
            view.decisions
                .iter()
                .flat_map(|decision| &decision.conditions)
                .filter(|condition| condition.assertion_covered)
                .count(),
        )?,
    );

    let gaps = file_gaps(view, selected)?;
    put_u64(
        &mut record,
        320,
        usize_u64(
            gaps.iter()
                .filter(|(_, gap)| {
                    gap.uncovered_lines
                        + gap.uncovered_functions * 2
                        + gap.missing_branches * 2
                        + gap.missing_mcdc_conditions * 3
                        + gap.measurement_limitations * 3
                        > 0
                })
                .count(),
        )?,
    );
    put_u64(
        &mut record,
        328,
        usize_u64(
            gaps.iter()
                .filter(|(_, gap)| {
                    gap.uncovered_lines > 0
                        || gap.uncovered_statements > 0
                        || gap.uncovered_functions > 0
                        || gap.missing_branches > 0
                        || gap.missing_mcdc_conditions > 0
                })
                .count(),
        )?,
    );

    let selected_tests = view
        .tests
        .iter()
        .filter(|test| selected.is_none_or(|selected| selected.contains(&test.id)))
        .collect::<Vec<_>>();
    put_u64(
        &mut record,
        336,
        usize_u64(
            selected_tests
                .iter()
                .filter(|test| test.role == "test")
                .count(),
        )?,
    );
    put_u64(
        &mut record,
        344,
        usize_u64(
            selected_tests
                .iter()
                .filter(|test| test.role == "setup")
                .count(),
        )?,
    );
    for (index, outcome) in [
        "passed",
        "failed",
        "flaky",
        "skipped",
        "timedOut",
        "interrupted",
        "unknown",
    ]
    .into_iter()
    .enumerate()
    {
        put_u64(
            &mut record,
            352 + index * 8,
            usize_u64(
                selected_tests
                    .iter()
                    .filter(|test| test.role == "test" && test.outcome == outcome)
                    .count(),
            )?,
        );
    }

    if let Some(transport) = &view.transport {
        record[1] = 1;
        for (index, value) in [
            transport.processes,
            transport.child_launches,
            transport.remote_launches,
            transport.workspace_capabilities,
            transport.scoped_server_records,
            transport.background_server_records,
            transport.corrupt_records,
            transport.corrupt_files,
        ]
        .into_iter()
        .enumerate()
        {
            put_u64(&mut record, 408 + index * 8, usize_u64(value)?);
        }
    }

    let phase_tests = view
        .phases
        .iter()
        .map(|phase| phase.test.as_str())
        .collect::<BTreeSet<_>>();
    let empty_tests = selected_tests
        .iter()
        .filter(|test| {
            test.role == "test"
                && test.lines.is_empty()
                && test.hits.is_empty()
                && test.decisions.is_empty()
                && phase_tests.contains(test.id.as_str())
        })
        .collect::<Vec<_>>();
    put_u64(&mut record, 472, usize_u64(empty_tests.len())?);
    put_u32(
        &mut record,
        20,
        empty_tests
            .first()
            .map_or(Ok(NO_STRING), |test| strings.intern(&test.name))?,
    );

    let entries = scope
        .and_then(|value| value.get("entries"))
        .and_then(serde_json::Value::as_array);
    for (index, status) in ["included", "excluded", "ambiguous"]
        .into_iter()
        .enumerate()
    {
        put_u64(
            &mut record,
            480 + index * 8,
            usize_u64(entries.map_or(0, |entries| {
                entries
                    .iter()
                    .filter(|entry| {
                        entry.get("status").and_then(serde_json::Value::as_str) == Some(status)
                    })
                    .count()
            }))?,
        );
    }
    Ok(record)
}

fn scope_entry_records(
    view_id: CoverageViewId,
    view: &CoverageView,
    strings: &mut StringTable,
) -> Result<Vec<[u8; SCOPE_ENTRY_RECORD_SIZE]>, CoverageIndexError> {
    let Some(scope) = &view.scope else {
        return Ok(Vec::new());
    };
    let entries = scope
        .get("entries")
        .and_then(serde_json::Value::as_array)
        .ok_or(CoverageIndexError::InvalidRecord("source-scope entries"))?;
    let mut limitations = BTreeMap::<&str, (usize, u32)>::new();
    for limitation in &view.limitations {
        let Some((file, kind)) = limitation_kind(limitation) else {
            return Err(CoverageIndexError::InvalidRecord("coverage limitation"));
        };
        let value = limitations.entry(file).or_default();
        value.0 += 1;
        value.1 |= match kind {
            "dynamic-code" => 1,
            "semantic-safety" => 2,
            "source-scope" => 4,
            _ => {
                return Err(CoverageIndexError::InvalidRecord(
                    "coverage limitation kind",
                ));
            }
        };
    }
    entries
        .iter()
        .map(|entry| {
            let file = entry
                .get("file")
                .and_then(serde_json::Value::as_str)
                .ok_or(CoverageIndexError::InvalidRecord("source-scope file"))?;
            let status = entry
                .get("status")
                .and_then(serde_json::Value::as_str)
                .ok_or(CoverageIndexError::InvalidRecord("source-scope status"))?;
            let reason = entry
                .get("reason")
                .and_then(serde_json::Value::as_str)
                .ok_or(CoverageIndexError::InvalidRecord("source-scope reason"))?;
            let package_root = entry
                .get("packageRoot")
                .map(|value| {
                    value.as_str().ok_or(CoverageIndexError::InvalidRecord(
                        "source-scope package root",
                    ))
                })
                .transpose()?;
            let mut record = [0_u8; SCOPE_ENTRY_RECORD_SIZE];
            record[0] = view_id as u8;
            record[1] = match status {
                "included" => 0,
                "excluded" => 1,
                "ambiguous" => 2,
                _ => return Err(CoverageIndexError::InvalidRecord("source-scope status")),
            };
            put_u32(&mut record, 4, strings.intern(file)?);
            put_u32(&mut record, 8, strings.intern(reason)?);
            put_u32(
                &mut record,
                12,
                package_root.map_or(Ok(NO_STRING), |value| strings.intern(value))?,
            );
            let (count, mask) = limitations.get(file).copied().unwrap_or_default();
            put_u64(&mut record, 16, usize_u64(count)?);
            put_u32(&mut record, 24, mask);
            Ok(record)
        })
        .collect()
}

fn optional_string_id(
    value: Option<&str>,
    strings: &mut StringTable,
) -> Result<u32, CoverageIndexError> {
    value.map_or(Ok(NO_STRING), |value| strings.intern(value))
}

fn confidence_record(
    confidence: &crate::coverage_report::CoverageConfidence,
    strings: &mut StringTable,
    relations: &mut StringRelations,
) -> Result<[u8; CONFIDENCE_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; CONFIDENCE_RECORD_SIZE];
    record[0] = match confidence.level.as_str() {
        "unexecuted" => 0,
        "executed" => 1,
        "action" => 2,
        "asserted" => 3,
        _ => return Err(CoverageIndexError::InvalidRecord("confidence level")),
    };
    record[1] = u8::from(confidence.setup_only)
        | (u8::from(confidence.background_only) << 1)
        | (u8::from(confidence.asserted) << 2)
        | (u8::from(confidence.e2e) << 3);
    for (index, values) in [
        confidence.tests.clone(),
        confidence.asserted_tests.clone(),
        confidence.runners.clone(),
        confidence.kinds.clone(),
    ]
    .into_iter()
    .enumerate()
    {
        let (offset, count) = relations.push(values, strings)?;
        put_u64(&mut record, 8 + index * 16, offset);
        put_u64(&mut record, 16 + index * 16, count);
    }
    Ok(record)
}

fn line_record(
    view_id: CoverageViewId,
    line: &crate::coverage_report::LineResult,
    confidence_index: usize,
    strings: &mut StringTable,
    relations: &mut StringRelations,
) -> Result<[u8; LINE_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; LINE_RECORD_SIZE];
    record[0] = view_id as u8;
    record[1] = u8::from(line.covered);
    put_u32(&mut record, 4, strings.intern(&line.file)?);
    put_u64(&mut record, 8, usize_u64(line.line)?);
    let (tests_offset, tests_count) = relations.push(line.tests.clone(), strings)?;
    put_u64(&mut record, 16, tests_offset);
    put_u64(&mut record, 24, tests_count);
    let (phases_offset, phases_count) = relations.push(line.phases.clone(), strings)?;
    put_u64(&mut record, 32, phases_offset);
    put_u64(&mut record, 40, phases_count);
    put_u64(&mut record, 48, usize_u64(confidence_index)?);
    Ok(record)
}

fn test_summary_record(
    view_id: CoverageViewId,
    test: &crate::coverage_report::TestCoverageResult,
    strings: &mut StringTable,
) -> Result<[u8; TEST_SUMMARY_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; TEST_SUMMARY_RECORD_SIZE];
    record[0] = view_id as u8;
    record[1] = match test.role.as_str() {
        "test" => 0,
        "setup" => 1,
        "background" => 2,
        _ => return Err(CoverageIndexError::InvalidRecord("test role")),
    };
    record[2] = match test.outcome.as_str() {
        "passed" => 0,
        "failed" => 1,
        "flaky" => 2,
        "skipped" => 3,
        "timedOut" => 4,
        "interrupted" => 5,
        "unknown" => 6,
        _ => return Err(CoverageIndexError::InvalidRecord("test outcome")),
    };
    put_u32(&mut record, 4, strings.intern(&test.id)?);
    put_u32(&mut record, 8, strings.intern(&test.name)?);
    put_u32(
        &mut record,
        12,
        optional_string_id(test.file.as_deref(), strings)?,
    );
    put_u32(
        &mut record,
        16,
        optional_string_id(test.title.as_deref(), strings)?,
    );
    put_u32(&mut record, 20, strings.intern(&test.provenance.runner)?);
    put_u32(&mut record, 24, strings.intern(&test.provenance.kind)?);
    put_u32(
        &mut record,
        28,
        optional_string_id(test.provenance.project.as_deref(), strings)?,
    );
    put_u32(&mut record, 32, strings.intern(&test.provenance.source)?);
    Ok(record)
}

fn phase_summary_record(
    view_id: CoverageViewId,
    phase: &crate::coverage_report::PhaseResult,
    strings: &mut StringTable,
) -> Result<[u8; PHASE_SUMMARY_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; PHASE_SUMMARY_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(&mut record, 4, strings.intern(&phase.phase.id)?);
    put_u32(&mut record, 8, strings.intern(&phase.phase.kind)?);
    put_u32(&mut record, 12, strings.intern(&phase.phase.operation)?);
    put_u32(
        &mut record,
        16,
        optional_string_id(phase.phase.source.as_deref(), strings)?,
    );
    put_u32(&mut record, 20, strings.intern(&phase.test)?);
    put_u32(
        &mut record,
        24,
        optional_string_id(phase.phase.status.as_deref(), strings)?,
    );
    put_u32(
        &mut record,
        28,
        optional_string_id(phase.phase.caused_by_phase_id.as_deref(), strings)?,
    );
    put_u64(&mut record, 32, usize_u64(phase.lines.len())?);
    put_u64(
        &mut record,
        40,
        usize_u64(
            phase
                .decisions
                .iter()
                .map(|decision| decision.vectors.len())
                .sum(),
        )?,
    );
    Ok(record)
}

struct AnchorInput<'a> {
    view_id: CoverageViewId,
    kind: u8,
    id: &'a str,
    file: &'a str,
    line: usize,
    column: usize,
    covered: bool,
    conditions: Option<(usize, usize)>,
    tests: &'a [String],
}

fn anchor_record(
    input: AnchorInput<'_>,
    strings: &mut StringTable,
    relations: &mut StringRelations,
) -> Result<[u8; ANCHOR_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; ANCHOR_RECORD_SIZE];
    record[0] = input.view_id as u8;
    record[1] = input.kind;
    record[2] = u8::from(input.covered);
    put_u32(&mut record, 4, strings.intern(input.id)?);
    put_u32(&mut record, 8, strings.intern(input.file)?);
    put_u64(&mut record, 16, usize_u64(input.line)?);
    put_u64(&mut record, 24, usize_u64(input.column)?);
    if let Some((covered, total)) = input.conditions {
        put_u64(&mut record, 32, usize_u64(total)?);
        put_u64(&mut record, 40, usize_u64(covered)?);
    }
    let (tests_offset, tests_count) = relations.push(input.tests.iter().cloned(), strings)?;
    put_u64(&mut record, 48, tests_offset);
    put_u64(&mut record, 56, tests_count);
    Ok(record)
}

fn test_retry_record(
    view_id: CoverageViewId,
    test_id: &str,
    retry: usize,
    strings: &mut StringTable,
) -> Result<[u8; TEST_RETRY_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; TEST_RETRY_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(&mut record, 4, strings.intern(test_id)?);
    put_u64(&mut record, 8, usize_u64(retry)?);
    Ok(record)
}

fn test_attempt_record(
    view_id: CoverageViewId,
    test_id: &str,
    attempt: &crate::coverage_report::TestAttempt,
    strings: &mut StringTable,
) -> Result<[u8; TEST_ATTEMPT_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; TEST_ATTEMPT_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(&mut record, 4, strings.intern(test_id)?);
    put_u64(&mut record, 8, usize_u64(attempt.retry)?);
    put_u32(&mut record, 16, strings.intern(&attempt.status)?);
    put_u32(
        &mut record,
        20,
        optional_string_id(attempt.expected_status.as_deref(), strings)?,
    );
    Ok(record)
}

fn test_line_record(
    view_id: CoverageViewId,
    test_id: &str,
    line: &crate::coverage_report::SourceLine,
    strings: &mut StringTable,
) -> Result<[u8; TEST_LINE_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; TEST_LINE_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(&mut record, 4, strings.intern(test_id)?);
    put_u32(&mut record, 8, strings.intern(&line.file)?);
    put_u64(&mut record, 16, usize_u64(line.line)?);
    Ok(record)
}

fn test_hit_record(
    view_id: CoverageViewId,
    test_id: &str,
    hit: &str,
    strings: &mut StringTable,
) -> Result<[u8; TEST_HIT_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; TEST_HIT_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(&mut record, 4, strings.intern(test_id)?);
    put_u32(&mut record, 8, strings.intern(hit)?);
    Ok(record)
}

fn test_vector_record(
    vector: &McdcVector,
    values: &mut Vec<u8>,
) -> Result<[u8; TEST_VECTOR_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; TEST_VECTOR_RECORD_SIZE];
    record[0] = u8::from(vector.outcome);
    put_u64(&mut record, 8, usize_u64(values.len())?);
    put_u64(&mut record, 16, usize_u64(vector.values.len())?);
    values.extend(vector.values.iter().map(|value| match value {
        None => 0,
        Some(false) => 1,
        Some(true) => 2,
    }));
    Ok(record)
}

fn test_decision_record(
    view_id: CoverageViewId,
    test_id: &str,
    decision_id: &str,
    vectors_offset: usize,
    vectors_count: usize,
    strings: &mut StringTable,
) -> Result<[u8; TEST_DECISION_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; TEST_DECISION_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(&mut record, 4, strings.intern(test_id)?);
    put_u32(&mut record, 8, strings.intern(decision_id)?);
    put_u64(&mut record, 16, usize_u64(vectors_offset)?);
    put_u64(&mut record, 24, usize_u64(vectors_count)?);
    Ok(record)
}

struct HitMetadataInput<'a> {
    view_id: CoverageViewId,
    kind: u8,
    id: &'a str,
    file: &'a str,
    line: usize,
    column: usize,
    branch_kind: Option<&'a str>,
    label: Option<&'a str>,
    alternative: Option<&'a str>,
}

fn hit_metadata_record(
    input: HitMetadataInput<'_>,
    strings: &mut StringTable,
) -> Result<[u8; HIT_METADATA_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; HIT_METADATA_RECORD_SIZE];
    record[0] = input.view_id as u8;
    record[1] = input.kind;
    put_u32(&mut record, 4, strings.intern(input.id)?);
    put_u32(&mut record, 8, strings.intern(input.file)?);
    put_u64(&mut record, 16, usize_u64(input.line)?);
    put_u64(&mut record, 24, usize_u64(input.column)?);
    put_u32(
        &mut record,
        32,
        optional_string_id(input.branch_kind, strings)?,
    );
    put_u32(&mut record, 36, optional_string_id(input.label, strings)?);
    put_u32(
        &mut record,
        40,
        optional_string_id(input.alternative, strings)?,
    );
    Ok(record)
}

fn decision_metadata_record(
    view_id: CoverageViewId,
    decision: &crate::coverage_report::DecisionResult,
    strings: &mut StringTable,
    relations: &mut StringRelations,
) -> Result<[u8; DECISION_METADATA_RECORD_SIZE], CoverageIndexError> {
    let mut record = [0_u8; DECISION_METADATA_RECORD_SIZE];
    record[0] = view_id as u8;
    put_u32(&mut record, 4, strings.intern(&decision.meta.id)?);
    put_u32(&mut record, 8, strings.intern(&decision.meta.file)?);
    put_u32(&mut record, 12, strings.intern(&decision.meta.source)?);
    put_u32(&mut record, 16, strings.intern(&decision.meta.kind)?);
    put_u64(&mut record, 24, usize_u64(decision.meta.line)?);
    put_u64(&mut record, 32, usize_u64(decision.meta.column)?);
    let (conditions_offset, conditions_count) =
        relations.push(decision.meta.conditions.clone(), strings)?;
    put_u64(&mut record, 40, conditions_offset);
    put_u64(&mut record, 48, conditions_count);
    Ok(record)
}

pub fn coverage_index_sections(
    report: &CoverageReport,
) -> Result<Vec<QueryIndexSection>, CoverageIndexError> {
    let views = [
        (CoverageViewId::All, &report.view),
        (CoverageViewId::Passed, &report.filters.passed),
        (CoverageViewId::Failed, &report.filters.failed),
    ];
    let mut strings = StringTable::default();
    let mut relations = StringRelations::default();
    let mut summaries = Vec::with_capacity(views.len() * SUMMARY_RECORD_SIZE);
    let mut gaps = Vec::new();
    let mut decision_gaps = Vec::new();
    let mut dimensions = Vec::new();
    let mut projection_records = Vec::new();
    let mut scope_entries = Vec::new();
    let mut confidence_records = Vec::new();
    let mut line_records = Vec::new();
    let mut test_summaries = Vec::new();
    let mut phase_summaries = Vec::new();
    let mut anchors = Vec::new();
    let mut test_retries = Vec::new();
    let mut test_attempts = Vec::new();
    let mut test_lines = Vec::new();
    let mut test_hits = Vec::new();
    let mut test_decisions = Vec::new();
    let mut test_vectors = Vec::new();
    let mut vector_values = Vec::new();
    let mut hit_metadata = Vec::new();
    let mut decision_metadata = Vec::new();
    for (id, view) in views {
        summaries.extend_from_slice(&summary_record(id, view, &mut strings)?);
        projection_records.extend_from_slice(&projection_record(
            id,
            view,
            None,
            None,
            None,
            &mut strings,
            &mut relations,
        )?);
        for entry in scope_entry_records(id, view, &mut strings)? {
            scope_entries.extend_from_slice(&entry);
        }
        for line in &view.lines {
            let confidence_index = confidence_records.len() / CONFIDENCE_RECORD_SIZE;
            confidence_records.extend_from_slice(&confidence_record(
                &line.confidence,
                &mut strings,
                &mut relations,
            )?);
            line_records.extend_from_slice(&line_record(
                id,
                line,
                confidence_index,
                &mut strings,
                &mut relations,
            )?);
        }
        for test in &view.tests {
            test_summaries.extend_from_slice(&test_summary_record(id, test, &mut strings)?);
            for retry in &test.retries {
                test_retries.extend_from_slice(&test_retry_record(
                    id,
                    &test.id,
                    *retry,
                    &mut strings,
                )?);
            }
            for attempt in &test.attempts {
                test_attempts.extend_from_slice(&test_attempt_record(
                    id,
                    &test.id,
                    attempt,
                    &mut strings,
                )?);
            }
            for line in &test.lines {
                test_lines.extend_from_slice(&test_line_record(id, &test.id, line, &mut strings)?);
            }
            for hit in &test.hits {
                test_hits.extend_from_slice(&test_hit_record(id, &test.id, hit, &mut strings)?);
            }
            for decision in &test.decisions {
                let vectors_offset = test_vectors.len() / TEST_VECTOR_RECORD_SIZE;
                for vector in &decision.vectors {
                    test_vectors
                        .extend_from_slice(&test_vector_record(vector, &mut vector_values)?);
                }
                test_decisions.extend_from_slice(&test_decision_record(
                    id,
                    &test.id,
                    &decision.id,
                    vectors_offset,
                    decision.vectors.len(),
                    &mut strings,
                )?);
            }
        }
        for phase in &view.phases {
            phase_summaries.extend_from_slice(&phase_summary_record(id, phase, &mut strings)?);
        }
        for decision in &view.decisions {
            decision_metadata.extend_from_slice(&decision_metadata_record(
                id,
                decision,
                &mut strings,
                &mut relations,
            )?);
            anchors.extend_from_slice(&anchor_record(
                AnchorInput {
                    view_id: id,
                    kind: 0,
                    id: &decision.meta.id,
                    file: &decision.meta.file,
                    line: decision.meta.line,
                    column: decision.meta.column,
                    covered: decision.covered,
                    conditions: Some((
                        decision
                            .conditions
                            .iter()
                            .filter(|condition| condition.covered)
                            .count(),
                        decision.conditions.len(),
                    )),
                    tests: &decision.tests,
                },
                &mut strings,
                &mut relations,
            )?);
        }
        for branch in &view.branches {
            anchors.extend_from_slice(&anchor_record(
                AnchorInput {
                    view_id: id,
                    kind: 1,
                    id: &branch.meta.id,
                    file: &branch.meta.file,
                    line: branch.meta.line,
                    column: branch.meta.column,
                    covered: branch.covered,
                    conditions: None,
                    tests: &[],
                },
                &mut strings,
                &mut relations,
            )?);
            for alternative in &branch.alternatives {
                hit_metadata.extend_from_slice(&hit_metadata_record(
                    HitMetadataInput {
                        view_id: id,
                        kind: 2,
                        id: &alternative.id,
                        file: &branch.meta.file,
                        line: branch.meta.line,
                        column: branch.meta.column,
                        branch_kind: Some(&branch.meta.kind),
                        label: None,
                        alternative: Some(&alternative.label),
                    },
                    &mut strings,
                )?);
            }
        }
        for point in &view.points {
            anchors.extend_from_slice(&anchor_record(
                AnchorInput {
                    view_id: id,
                    kind: match point.meta.kind {
                        crate::coverage_analysis::PointKind::Statement => 2,
                        crate::coverage_analysis::PointKind::Function => 3,
                    },
                    id: &point.meta.id,
                    file: &point.meta.file,
                    line: point.meta.line,
                    column: point.meta.column,
                    covered: point.covered,
                    conditions: None,
                    tests: &point.tests,
                },
                &mut strings,
                &mut relations,
            )?);
            hit_metadata.extend_from_slice(&hit_metadata_record(
                HitMetadataInput {
                    view_id: id,
                    kind: match point.meta.kind {
                        crate::coverage_analysis::PointKind::Statement => 0,
                        crate::coverage_analysis::PointKind::Function => 1,
                    },
                    id: &point.meta.id,
                    file: &point.meta.file,
                    line: point.meta.line,
                    column: point.meta.column,
                    branch_kind: None,
                    label: point.meta.label.as_deref(),
                    alternative: None,
                },
                &mut strings,
            )?);
        }
        for value in &view.coverage_by_kind {
            dimensions.extend_from_slice(&dimension_record(
                id,
                CoverageDimension::Kind,
                value,
                &mut strings,
            )?);
        }
        for value in &view.coverage_by_runner {
            dimensions.extend_from_slice(&dimension_record(
                id,
                CoverageDimension::Runner,
                value,
                &mut strings,
            )?);
        }
        for decision in &view.decisions {
            decision_gaps.extend_from_slice(&decision_gap_record(
                id,
                decision,
                None,
                None,
                None,
                &mut strings,
            )?);
        }
        for (file, gap) in file_gaps(view, None)? {
            gaps.extend_from_slice(&file_gap_record(id, &file, &gap, None, None, &mut strings)?);
        }
        for (kind, runner, selected) in projections(view) {
            projection_records.extend_from_slice(&projection_record(
                id,
                view,
                Some(&selected),
                kind.as_deref(),
                runner.as_deref(),
                &mut strings,
                &mut relations,
            )?);
            for decision in &view.decisions {
                decision_gaps.extend_from_slice(&decision_gap_record(
                    id,
                    decision,
                    Some(&selected),
                    kind.as_deref(),
                    runner.as_deref(),
                    &mut strings,
                )?);
            }
            for (file, gap) in file_gaps(view, Some(&selected))? {
                gaps.extend_from_slice(&file_gap_record(
                    id,
                    &file,
                    &gap,
                    kind.as_deref(),
                    runner.as_deref(),
                    &mut strings,
                )?);
            }
        }
    }
    let [blob, string_records] = strings.sections()?;
    let string_relations = relations.section()?;
    Ok(vec![
        blob,
        string_records,
        string_relations,
        QueryIndexSection {
            kind: SECTION_VIEW_SUMMARIES,
            record_size: SUMMARY_RECORD_SIZE as u32,
            count: usize_u64(summaries.len() / SUMMARY_RECORD_SIZE)?,
            bytes: summaries,
        },
        QueryIndexSection {
            kind: SECTION_FILE_GAPS,
            record_size: FILE_GAP_RECORD_SIZE as u32,
            count: usize_u64(gaps.len() / FILE_GAP_RECORD_SIZE)?,
            bytes: gaps,
        },
        QueryIndexSection {
            kind: SECTION_DECISION_GAPS,
            record_size: DECISION_GAP_RECORD_SIZE as u32,
            count: usize_u64(decision_gaps.len() / DECISION_GAP_RECORD_SIZE)?,
            bytes: decision_gaps,
        },
        QueryIndexSection {
            kind: SECTION_DIMENSIONS,
            record_size: DIMENSION_RECORD_SIZE as u32,
            count: usize_u64(dimensions.len() / DIMENSION_RECORD_SIZE)?,
            bytes: dimensions,
        },
        QueryIndexSection {
            kind: SECTION_PROJECTIONS,
            record_size: PROJECTION_RECORD_SIZE as u32,
            count: usize_u64(projection_records.len() / PROJECTION_RECORD_SIZE)?,
            bytes: projection_records,
        },
        QueryIndexSection {
            kind: SECTION_SCOPE_ENTRIES,
            record_size: SCOPE_ENTRY_RECORD_SIZE as u32,
            count: usize_u64(scope_entries.len() / SCOPE_ENTRY_RECORD_SIZE)?,
            bytes: scope_entries,
        },
        QueryIndexSection {
            kind: SECTION_CONFIDENCE,
            record_size: CONFIDENCE_RECORD_SIZE as u32,
            count: usize_u64(confidence_records.len() / CONFIDENCE_RECORD_SIZE)?,
            bytes: confidence_records,
        },
        QueryIndexSection {
            kind: SECTION_LINES,
            record_size: LINE_RECORD_SIZE as u32,
            count: usize_u64(line_records.len() / LINE_RECORD_SIZE)?,
            bytes: line_records,
        },
        QueryIndexSection {
            kind: SECTION_TEST_SUMMARIES,
            record_size: TEST_SUMMARY_RECORD_SIZE as u32,
            count: usize_u64(test_summaries.len() / TEST_SUMMARY_RECORD_SIZE)?,
            bytes: test_summaries,
        },
        QueryIndexSection {
            kind: SECTION_PHASE_SUMMARIES,
            record_size: PHASE_SUMMARY_RECORD_SIZE as u32,
            count: usize_u64(phase_summaries.len() / PHASE_SUMMARY_RECORD_SIZE)?,
            bytes: phase_summaries,
        },
        QueryIndexSection {
            kind: SECTION_ANCHORS,
            record_size: ANCHOR_RECORD_SIZE as u32,
            count: usize_u64(anchors.len() / ANCHOR_RECORD_SIZE)?,
            bytes: anchors,
        },
        QueryIndexSection {
            kind: SECTION_TEST_RETRIES,
            record_size: TEST_RETRY_RECORD_SIZE as u32,
            count: usize_u64(test_retries.len() / TEST_RETRY_RECORD_SIZE)?,
            bytes: test_retries,
        },
        QueryIndexSection {
            kind: SECTION_TEST_ATTEMPTS,
            record_size: TEST_ATTEMPT_RECORD_SIZE as u32,
            count: usize_u64(test_attempts.len() / TEST_ATTEMPT_RECORD_SIZE)?,
            bytes: test_attempts,
        },
        QueryIndexSection {
            kind: SECTION_TEST_LINES,
            record_size: TEST_LINE_RECORD_SIZE as u32,
            count: usize_u64(test_lines.len() / TEST_LINE_RECORD_SIZE)?,
            bytes: test_lines,
        },
        QueryIndexSection {
            kind: SECTION_TEST_HITS,
            record_size: TEST_HIT_RECORD_SIZE as u32,
            count: usize_u64(test_hits.len() / TEST_HIT_RECORD_SIZE)?,
            bytes: test_hits,
        },
        QueryIndexSection {
            kind: SECTION_TEST_DECISIONS,
            record_size: TEST_DECISION_RECORD_SIZE as u32,
            count: usize_u64(test_decisions.len() / TEST_DECISION_RECORD_SIZE)?,
            bytes: test_decisions,
        },
        QueryIndexSection {
            kind: SECTION_TEST_VECTORS,
            record_size: TEST_VECTOR_RECORD_SIZE as u32,
            count: usize_u64(test_vectors.len() / TEST_VECTOR_RECORD_SIZE)?,
            bytes: test_vectors,
        },
        QueryIndexSection {
            kind: SECTION_VECTOR_VALUES,
            record_size: 1,
            count: usize_u64(vector_values.len())?,
            bytes: vector_values,
        },
        QueryIndexSection {
            kind: SECTION_HIT_METADATA,
            record_size: HIT_METADATA_RECORD_SIZE as u32,
            count: usize_u64(hit_metadata.len() / HIT_METADATA_RECORD_SIZE)?,
            bytes: hit_metadata,
        },
        QueryIndexSection {
            kind: SECTION_DECISION_METADATA,
            record_size: DECISION_METADATA_RECORD_SIZE as u32,
            count: usize_u64(decision_metadata.len() / DECISION_METADATA_RECORD_SIZE)?,
            bytes: decision_metadata,
        },
    ])
}

pub struct CoverageIndex<'a> {
    index: &'a QueryIndex,
}

impl<'a> CoverageIndex<'a> {
    pub fn new(index: &'a QueryIndex) -> Result<Self, CoverageIndexError> {
        for (kind, size) in [
            (SECTION_STRINGS, STRING_RECORD_SIZE),
            (SECTION_VIEW_SUMMARIES, SUMMARY_RECORD_SIZE),
            (SECTION_FILE_GAPS, FILE_GAP_RECORD_SIZE),
            (SECTION_DECISION_GAPS, DECISION_GAP_RECORD_SIZE),
            (SECTION_DIMENSIONS, DIMENSION_RECORD_SIZE),
            (SECTION_PROJECTIONS, PROJECTION_RECORD_SIZE),
            (SECTION_SCOPE_ENTRIES, SCOPE_ENTRY_RECORD_SIZE),
            (SECTION_CONFIDENCE, CONFIDENCE_RECORD_SIZE),
            (SECTION_LINES, LINE_RECORD_SIZE),
            (SECTION_TEST_SUMMARIES, TEST_SUMMARY_RECORD_SIZE),
            (SECTION_PHASE_SUMMARIES, PHASE_SUMMARY_RECORD_SIZE),
            (SECTION_ANCHORS, ANCHOR_RECORD_SIZE),
            (SECTION_TEST_RETRIES, TEST_RETRY_RECORD_SIZE),
            (SECTION_TEST_ATTEMPTS, TEST_ATTEMPT_RECORD_SIZE),
            (SECTION_TEST_LINES, TEST_LINE_RECORD_SIZE),
            (SECTION_TEST_HITS, TEST_HIT_RECORD_SIZE),
            (SECTION_TEST_DECISIONS, TEST_DECISION_RECORD_SIZE),
            (SECTION_TEST_VECTORS, TEST_VECTOR_RECORD_SIZE),
            (SECTION_VECTOR_VALUES, 1),
            (SECTION_HIT_METADATA, HIT_METADATA_RECORD_SIZE),
            (SECTION_DECISION_METADATA, DECISION_METADATA_RECORD_SIZE),
        ] {
            if index.descriptor(kind)?.record_size as usize != size {
                return Err(CoverageIndexError::InvalidRecord("record size"));
            }
        }
        index.descriptor(SECTION_STRING_BYTES)?;
        if index.descriptor(SECTION_STRING_RELATIONS)?.record_size != 4 {
            return Err(CoverageIndexError::InvalidRecord(
                "string-relation record size",
            ));
        }
        Ok(Self { index })
    }

    fn string(&self, id: u32) -> Result<String, CoverageIndexError> {
        let record = self.index.record(SECTION_STRINGS, u64::from(id))?;
        if record[12..].iter().any(|byte| *byte != 0) {
            return Err(CoverageIndexError::InvalidRecord("string reserved bytes"));
        }
        let offset = get_u64(record, 0)?;
        let length = u64::from(get_u32(record, 8)?);
        let value = self.index.bytes(SECTION_STRING_BYTES, offset, length)?;
        std::str::from_utf8(value)
            .map(str::to_owned)
            .map_err(|_| CoverageIndexError::InvalidUtf8)
    }

    pub fn summary(&self, view: CoverageViewId) -> Result<CoverageSummary, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_VIEW_SUMMARIES)?;
        if descriptor.count != 3 {
            return Err(CoverageIndexError::InvalidRecord("summary view count"));
        }
        let mut found = None;
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_VIEW_SUMMARIES, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[3] != 0 || record[12..16].iter().any(|byte| *byte != 0) {
                return Err(CoverageIndexError::InvalidRecord("summary reserved bytes"));
            }
            self.string(get_u32(record, 4)?)?;
            self.string(get_u32(record, 8)?)?;
            let count = |offset: usize| -> Result<CoverageCount, CoverageIndexError> {
                let covered = usize::try_from(get_u64(record, offset)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?;
                let total = usize::try_from(get_u64(record, offset + 8)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?;
                if covered > total {
                    return Err(CoverageIndexError::InvalidRecord("covered exceeds total"));
                }
                Ok(CoverageCount {
                    covered,
                    total,
                    percentage: percentage(covered, total),
                })
            };
            let value = |offset: usize| -> Result<usize, CoverageIndexError> {
                usize::try_from(get_u64(record, offset)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)
            };
            let conditions = value(40)?;
            let covered_conditions = value(48)?;
            if covered_conditions > conditions {
                return Err(CoverageIndexError::InvalidRecord(
                    "covered conditions exceed total",
                ));
            }
            let decisions = value(16)?;
            let executed_decisions = value(24)?;
            let covered_decisions = value(32)?;
            if covered_decisions > executed_decisions || executed_decisions > decisions {
                return Err(CoverageIndexError::InvalidRecord("decision count ordering"));
            }
            let summary = CoverageSummary {
                decisions,
                executed_decisions,
                covered_decisions,
                conditions,
                covered_conditions,
                condition_coverage_pct: percentage(covered_conditions, conditions),
                lines: count(56)?,
                statements: count(72)?,
                functions: count(88)?,
                branches: count(104)?,
                decision_outcomes: count(120)?,
                condition_outcomes: count(136)?,
                value_selections: count(152)?,
                coverage_complete: bool_field(record[1])?,
                completeness_blocked: match record[2] {
                    0 => None,
                    1 => Some(false),
                    2 => Some(true),
                    _ => return Err(CoverageIndexError::InvalidRecord("optional boolean")),
                },
            };
            if found.replace(summary).is_some() {
                return Err(CoverageIndexError::InvalidRecord("duplicate coverage view"));
            }
        }
        found.ok_or(CoverageIndexError::InvalidRecord("missing coverage view"))
    }

    pub fn file_gaps(
        &self,
        view: CoverageViewId,
        kind: Option<&str>,
        runner: Option<&str>,
    ) -> Result<Vec<IndexedFileGap>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_FILE_GAPS)?;
        let mut gaps = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_FILE_GAPS, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[1..4].iter().any(|byte| *byte != 0)
                || record[60..64].iter().any(|byte| *byte != 0)
                || record[160..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord("file-gap reserved bytes"));
            }
            let number = |offset: usize| -> Result<usize, CoverageIndexError> {
                usize::try_from(get_u64(record, offset)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)
            };
            let mask = get_u32(record, 56)?;
            if mask & !15 != 0 {
                return Err(CoverageIndexError::InvalidRecord("limitation mask"));
            }
            let measurement_limitations = number(48)?;
            if (measurement_limitations == 0) != (mask == 0) {
                return Err(CoverageIndexError::InvalidRecord(
                    "limitation count and kinds disagree",
                ));
            }
            let uncovered_lines = number(8)?;
            let uncovered_statements = number(16)?;
            let uncovered_functions = number(24)?;
            let missing_branches = number(32)?;
            let missing_mcdc_conditions = number(40)?;
            let score = number(64)?;
            let expected_score = uncovered_lines
                + uncovered_functions * 2
                + missing_branches * 2
                + missing_mcdc_conditions * 3
                + measurement_limitations * 3;
            if score != expected_score {
                return Err(CoverageIndexError::InvalidRecord("file-gap score"));
            }
            let record_kind = self.optional_string(get_u32(record, 72)?)?;
            let record_runner = self.optional_string(get_u32(record, 76)?)?;
            if record_kind.as_deref() != kind || record_runner.as_deref() != runner {
                continue;
            }
            let mut limitation_kinds = Vec::new();
            for (bit, kind) in [
                (1, "dynamic-code"),
                (2, "semantic-safety"),
                (4, "source-scope"),
                (8, "unknown"),
            ] {
                if mask & bit != 0 {
                    limitation_kinds.push(kind.into());
                }
            }
            gaps.push(IndexedFileGap {
                view,
                file: self.string(get_u32(record, 4)?)?,
                uncovered_lines,
                uncovered_statements,
                uncovered_functions,
                missing_branches,
                missing_mcdc_conditions,
                measurement_limitations,
                limitation_kinds,
                covered_by_other_tests: IndexedGapDimensions {
                    lines: number(80)?,
                    statements: number(88)?,
                    functions: number(96)?,
                    branches: number(104)?,
                    mcdc_conditions: number(112)?,
                },
                uncovered_everywhere: IndexedGapDimensions {
                    lines: number(120)?,
                    statements: number(128)?,
                    functions: number(136)?,
                    branches: number(144)?,
                    mcdc_conditions: number(152)?,
                },
                score,
            });
        }
        gaps.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.file.cmp(&right.file))
        });
        Ok(gaps)
    }

    fn optional_string(&self, id: u32) -> Result<Option<String>, CoverageIndexError> {
        if id == NO_STRING {
            Ok(None)
        } else {
            self.string(id).map(Some)
        }
    }

    fn relation_strings(&self, offset: u64, count: u64) -> Result<Vec<String>, CoverageIndexError> {
        let end = offset
            .checked_add(count)
            .ok_or(CoverageIndexError::SizeOverflow)?;
        let descriptor = self.index.descriptor(SECTION_STRING_RELATIONS)?;
        if end > descriptor.count {
            return Err(CoverageIndexError::InvalidRecord("string relation range"));
        }
        (offset..end)
            .map(|index| {
                let record = self.index.record(SECTION_STRING_RELATIONS, index)?;
                self.string(get_u32(record, 0)?)
            })
            .collect()
    }

    pub fn projection(
        &self,
        view: CoverageViewId,
        kind: Option<&str>,
        runner: Option<&str>,
    ) -> Result<IndexedProjection, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_PROJECTIONS)?;
        let mut found = None;
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_PROJECTIONS, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[3] != 0
                || record[38..40].iter().any(|byte| *byte != 0)
                || record[504..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord(
                    "projection reserved bytes",
                ));
            }
            let record_kind = self.optional_string(get_u32(record, 4)?)?;
            let record_runner = self.optional_string(get_u32(record, 8)?)?;
            if record_kind.as_deref() != kind || record_runner.as_deref() != runner {
                continue;
            }
            if found.is_some() {
                return Err(CoverageIndexError::InvalidRecord(
                    "duplicate coverage projection",
                ));
            }
            let number = |offset: usize| -> Result<usize, CoverageIndexError> {
                usize::try_from(get_u64(record, offset)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)
            };
            let limitations = number(192)?;
            let evidence_corruptions = number(200)?;
            let blocking = number(208)?;
            if blocking != limitations + evidence_corruptions {
                return Err(CoverageIndexError::InvalidRecord(
                    "measurement blocking count",
                ));
            }
            let transport_values = (0..8)
                .map(|index| number(408 + index * 8))
                .collect::<Result<Vec<_>, _>>()?;
            let transport = if bool_field(record[1])? {
                Some(TransportStats {
                    processes: transport_values[0],
                    child_launches: transport_values[1],
                    remote_launches: transport_values[2],
                    workspace_capabilities: transport_values[3],
                    scoped_server_records: transport_values[4],
                    background_server_records: transport_values[5],
                    corrupt_records: transport_values[6],
                    corrupt_files: transport_values[7],
                })
            } else {
                if transport_values.iter().any(|value| *value != 0) {
                    return Err(CoverageIndexError::InvalidRecord("transport presence flag"));
                }
                None
            };
            let has_scope = bool_field(record[2])?;
            let scope_mode = self.optional_string(get_u32(record, 16)?)?;
            if has_scope != scope_mode.is_some() {
                return Err(CoverageIndexError::InvalidRecord("scope presence flag"));
            }
            let source_scope = if let Some(mode) = scope_mode {
                Some(IndexedSourceScope {
                    mode,
                    roots: self
                        .relation_strings(get_u64(record, 24)?, u64::from(get_u32(record, 32)?))?,
                    included: number(480)?,
                    excluded: number(488)?,
                    ambiguous: number(496)?,
                })
            } else {
                if get_u32(record, 32)? != 0
                    || number(480)? != 0
                    || number(488)? != 0
                    || number(496)? != 0
                {
                    return Err(CoverageIndexError::InvalidRecord("absent scope data"));
                }
                None
            };
            let empty_evidence_tests = number(472)?;
            let first_empty_evidence_test = self.optional_string(get_u32(record, 20)?)?;
            if (empty_evidence_tests == 0) != first_empty_evidence_test.is_none() {
                return Err(CoverageIndexError::InvalidRecord(
                    "empty-evidence diagnostic identity",
                ));
            }
            found = Some(IndexedProjection {
                view,
                kind: record_kind,
                runner: record_runner,
                generated_at: self.string(get_u32(record, 12)?)?,
                summary: decode_summary(record, 36, 40)?,
                measurement: IndexedMeasurement {
                    complete: blocking == 0,
                    limitations,
                    evidence_corruptions,
                    blocking,
                    files: number(216)?,
                    by_kind: IndexedMeasurementKinds {
                        dynamic_code: number(224)?,
                        semantic_safety: number(232)?,
                        source_scope: number(240)?,
                    },
                },
                attribution: IndexedAttribution {
                    browser_explicit: number(248)?,
                    browser_fallback: number(256)?,
                    server_explicit: number(264)?,
                    server_fallback: number(272)?,
                },
                transport,
                empty_evidence_tests,
                first_empty_evidence_test,
                confidence: IndexedSummaryConfidence {
                    lines: IndexedConfidenceLines {
                        unexecuted: number(280)?,
                        executed: number(288)?,
                        action: number(296)?,
                        asserted: number(304)?,
                    },
                    assertion_covered_mcdc_conditions: number(312)?,
                },
                files_with_gaps: number(320)?,
                files_with_coverage_gaps: number(328)?,
                tests: number(336)?,
                setups: number(344)?,
                test_outcomes: IndexedOutcomeCounts {
                    passed: number(352)?,
                    failed: number(360)?,
                    flaky: number(368)?,
                    skipped: number(376)?,
                    timed_out: number(384)?,
                    interrupted: number(392)?,
                    unknown: number(400)?,
                },
                source_scope,
            });
        }
        found.ok_or(CoverageIndexError::InvalidRecord(
            "missing coverage projection",
        ))
    }

    pub fn decision_gaps(
        &self,
        view: CoverageViewId,
        kind: Option<&str>,
        runner: Option<&str>,
        file: &str,
    ) -> Result<Vec<IndexedDecisionGap>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_DECISION_GAPS)?;
        let mut decisions = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_DECISION_GAPS, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[1..4].iter().any(|byte| *byte != 0)
                || record[28..32].iter().any(|byte| *byte != 0)
                || record[64..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord(
                    "decision-gap reserved bytes",
                ));
            }
            let record_kind = self.optional_string(get_u32(record, 4)?)?;
            let record_runner = self.optional_string(get_u32(record, 8)?)?;
            if record_kind.as_deref() != kind || record_runner.as_deref() != runner {
                continue;
            }
            let record_file = self.string(get_u32(record, 16)?)?;
            if record_file != file {
                continue;
            }
            let number = |offset: usize| -> Result<usize, CoverageIndexError> {
                usize::try_from(get_u64(record, offset)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)
            };
            let conditions = number(48)?;
            let missing_conditions = number(56)?;
            if conditions == 0 || missing_conditions > conditions {
                return Err(CoverageIndexError::InvalidRecord(
                    "decision condition counts",
                ));
            }
            decisions.push(IndexedDecisionGap {
                view,
                file: record_file,
                id: self.string(get_u32(record, 12)?)?,
                line: number(32)?,
                column: number(40)?,
                kind: self.string(get_u32(record, 20)?)?,
                conditions,
                missing_conditions,
                waived_conditions: 0,
                source: self.string(get_u32(record, 24)?)?,
            });
        }
        Ok(decisions)
    }

    pub fn dimensions(
        &self,
        view: CoverageViewId,
        dimension: CoverageDimension,
    ) -> Result<Vec<IndexedDimensionCoverage>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_DIMENSIONS)?;
        let mut values = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_DIMENSIONS, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            let record_dimension = match record[1] {
                0 => CoverageDimension::Kind,
                1 => CoverageDimension::Runner,
                _ => return Err(CoverageIndexError::InvalidRecord("dimension type")),
            };
            if record_dimension != dimension {
                continue;
            }
            if record[26..32].iter().any(|byte| *byte != 0)
                || record[184..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord(
                    "dimension reserved bytes",
                ));
            }
            let name = self.string(get_u32(record, 4)?)?;
            values.push(IndexedDimensionCoverage {
                kind: (dimension == CoverageDimension::Kind).then(|| name.clone()),
                runner: (dimension == CoverageDimension::Runner).then_some(name),
                tests: usize::try_from(get_u64(record, 8)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
                setups: usize::try_from(get_u64(record, 16)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
                summary: decode_summary(record, 24, 32)?,
            });
        }
        Ok(values)
    }

    pub fn scope_entries(
        &self,
        view: CoverageViewId,
    ) -> Result<Vec<IndexedScopeEntry>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_SCOPE_ENTRIES)?;
        let mut entries = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_SCOPE_ENTRIES, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[2..4].iter().any(|byte| *byte != 0)
                || record[28..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord(
                    "source-scope reserved bytes",
                ));
            }
            let status = match record[1] {
                0 => "included",
                1 => "excluded",
                2 => "ambiguous",
                _ => return Err(CoverageIndexError::InvalidRecord("source-scope status")),
            };
            let measurement_limitations = usize::try_from(get_u64(record, 16)?)
                .map_err(|_| CoverageIndexError::SizeOverflow)?;
            let mask = get_u32(record, 24)?;
            if mask & !7 != 0 || (measurement_limitations == 0) != (mask == 0) {
                return Err(CoverageIndexError::InvalidRecord(
                    "source-scope limitation annotation",
                ));
            }
            let mut limitation_kinds = Vec::new();
            for (bit, kind) in [
                (1, "dynamic-code"),
                (2, "semantic-safety"),
                (4, "source-scope"),
            ] {
                if mask & bit != 0 {
                    limitation_kinds.push(kind.into());
                }
            }
            entries.push(IndexedScopeEntry {
                file: self.string(get_u32(record, 4)?)?,
                status: status.into(),
                reason: self.string(get_u32(record, 8)?)?,
                package_root: self.optional_string(get_u32(record, 12)?)?,
                measurement_limitations,
                limitation_kinds,
            });
        }
        Ok(entries)
    }

    fn confidence(
        &self,
        index: u64,
    ) -> Result<crate::coverage_report::CoverageConfidence, CoverageIndexError> {
        let record = self.index.record(SECTION_CONFIDENCE, index)?;
        if record[2..8].iter().any(|byte| *byte != 0)
            || record[72..].iter().any(|byte| *byte != 0)
            || record[1] & !15 != 0
        {
            return Err(CoverageIndexError::InvalidRecord("confidence record"));
        }
        let values = (0..4)
            .map(|index| {
                self.relation_strings(
                    get_u64(record, 8 + index * 16)?,
                    get_u64(record, 16 + index * 16)?,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(crate::coverage_report::CoverageConfidence {
            level: match record[0] {
                0 => "unexecuted",
                1 => "executed",
                2 => "action",
                3 => "asserted",
                _ => return Err(CoverageIndexError::InvalidRecord("confidence level")),
            }
            .into(),
            setup_only: record[1] & 1 != 0,
            background_only: record[1] & 2 != 0,
            asserted: record[1] & 4 != 0,
            e2e: record[1] & 8 != 0,
            tests: values[0].clone(),
            asserted_tests: values[1].clone(),
            runners: values[2].clone(),
            kinds: values[3].clone(),
        })
    }

    pub fn line(
        &self,
        view: CoverageViewId,
        file: &str,
        line: usize,
    ) -> Result<Option<IndexedLine>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_LINES)?;
        let mut found = None;
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_LINES, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[2..4].iter().any(|byte| *byte != 0)
                || record[56..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord("line record"));
            }
            let record_file = self.string(get_u32(record, 4)?)?;
            let record_line = usize::try_from(get_u64(record, 8)?)
                .map_err(|_| CoverageIndexError::SizeOverflow)?;
            if record_file != file || record_line != line {
                continue;
            }
            if found.is_some() {
                return Err(CoverageIndexError::InvalidRecord("duplicate line"));
            }
            found = Some(IndexedLine {
                file: record_file,
                line: record_line,
                covered: bool_field(record[1])?,
                tests: self.relation_strings(get_u64(record, 16)?, get_u64(record, 24)?)?,
                phases: self.relation_strings(get_u64(record, 32)?, get_u64(record, 40)?)?,
                confidence: self.confidence(get_u64(record, 48)?)?,
            });
        }
        Ok(found)
    }

    pub fn test_summaries(
        &self,
        view: CoverageViewId,
    ) -> Result<Vec<IndexedTestSummary>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_TEST_SUMMARIES)?;
        let mut tests = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_TEST_SUMMARIES, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[3] != 0 || record[36..].iter().any(|byte| *byte != 0) {
                return Err(CoverageIndexError::InvalidRecord("test summary record"));
            }
            tests.push(IndexedTestSummary {
                id: self.string(get_u32(record, 4)?)?,
                name: self.string(get_u32(record, 8)?)?,
                file: self.optional_string(get_u32(record, 12)?)?,
                title: self.optional_string(get_u32(record, 16)?)?,
                role: match record[1] {
                    0 => "test",
                    1 => "setup",
                    2 => "background",
                    _ => return Err(CoverageIndexError::InvalidRecord("test role")),
                }
                .into(),
                outcome: match record[2] {
                    0 => "passed",
                    1 => "failed",
                    2 => "flaky",
                    3 => "skipped",
                    4 => "timedOut",
                    5 => "interrupted",
                    6 => "unknown",
                    _ => return Err(CoverageIndexError::InvalidRecord("test outcome")),
                }
                .into(),
                provenance: crate::coverage_report::TestProvenance {
                    runner: self.string(get_u32(record, 20)?)?,
                    kind: self.string(get_u32(record, 24)?)?,
                    project: self.optional_string(get_u32(record, 28)?)?,
                    source: self.string(get_u32(record, 32)?)?,
                },
            });
        }
        Ok(tests)
    }

    fn test_vector(&self, index: u64) -> Result<McdcVector, CoverageIndexError> {
        let record = self.index.record(SECTION_TEST_VECTORS, index)?;
        if record[1..8].iter().any(|byte| *byte != 0) {
            return Err(CoverageIndexError::InvalidRecord("test vector record"));
        }
        let offset = get_u64(record, 8)?;
        let count = get_u64(record, 16)?;
        let descriptor = self.index.descriptor(SECTION_VECTOR_VALUES)?;
        let end = offset
            .checked_add(count)
            .ok_or(CoverageIndexError::InvalidRecord("vector value range"))?;
        if end > descriptor.count {
            return Err(CoverageIndexError::InvalidRecord("vector value range"));
        }
        let mut values = Vec::with_capacity(
            usize::try_from(count).map_err(|_| CoverageIndexError::SizeOverflow)?,
        );
        for index in offset..end {
            values.push(match self.index.record(SECTION_VECTOR_VALUES, index)?[0] {
                0 => None,
                1 => Some(false),
                2 => Some(true),
                _ => return Err(CoverageIndexError::InvalidRecord("vector value")),
            });
        }
        Ok(McdcVector {
            values,
            outcome: bool_field(record[0])?,
        })
    }

    pub fn test_details(
        &self,
        view: CoverageViewId,
    ) -> Result<Vec<IndexedTestDetail>, CoverageIndexError> {
        let summaries = self.test_summaries(view)?;
        let positions = summaries
            .iter()
            .enumerate()
            .map(|(index, test)| (test.id.clone(), index))
            .collect::<HashMap<_, _>>();
        if positions.len() != summaries.len() {
            return Err(CoverageIndexError::InvalidRecord("duplicate test summary"));
        }
        let mut details = summaries
            .into_iter()
            .map(|summary| IndexedTestDetail {
                summary,
                retries: Vec::new(),
                attempts: Vec::new(),
                hits: Vec::new(),
                decisions: Vec::new(),
                lines: Vec::new(),
            })
            .collect::<Vec<_>>();
        let position = |record: &[u8]| -> Result<Option<usize>, CoverageIndexError> {
            if CoverageViewId::try_from(record[0])? != view {
                return Ok(None);
            }
            let id = self.string(get_u32(record, 4)?)?;
            positions
                .get(&id)
                .copied()
                .map(Some)
                .ok_or(CoverageIndexError::InvalidRecord("unknown test relation"))
        };
        let descriptor = self.index.descriptor(SECTION_TEST_RETRIES)?;
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_TEST_RETRIES, index)?;
            if record[1..4].iter().any(|byte| *byte != 0) {
                return Err(CoverageIndexError::InvalidRecord("test retry record"));
            }
            if let Some(position) = position(record)? {
                details[position].retries.push(
                    usize::try_from(get_u64(record, 8)?)
                        .map_err(|_| CoverageIndexError::SizeOverflow)?,
                );
            }
        }
        let descriptor = self.index.descriptor(SECTION_TEST_ATTEMPTS)?;
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_TEST_ATTEMPTS, index)?;
            if record[1..4].iter().any(|byte| *byte != 0) {
                return Err(CoverageIndexError::InvalidRecord("test attempt record"));
            }
            if let Some(position) = position(record)? {
                details[position]
                    .attempts
                    .push(crate::coverage_report::TestAttempt {
                        retry: usize::try_from(get_u64(record, 8)?)
                            .map_err(|_| CoverageIndexError::SizeOverflow)?,
                        status: self.string(get_u32(record, 16)?)?,
                        expected_status: self.optional_string(get_u32(record, 20)?)?,
                    });
            }
        }
        let descriptor = self.index.descriptor(SECTION_TEST_LINES)?;
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_TEST_LINES, index)?;
            if record[1..4].iter().any(|byte| *byte != 0)
                || record[12..16].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord("test line record"));
            }
            if let Some(position) = position(record)? {
                details[position]
                    .lines
                    .push(crate::coverage_report::SourceLine {
                        file: self.string(get_u32(record, 8)?)?,
                        line: usize::try_from(get_u64(record, 16)?)
                            .map_err(|_| CoverageIndexError::SizeOverflow)?,
                    });
            }
        }
        let descriptor = self.index.descriptor(SECTION_TEST_HITS)?;
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_TEST_HITS, index)?;
            if record[1..4].iter().any(|byte| *byte != 0)
                || record[12..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord("test hit record"));
            }
            if let Some(position) = position(record)? {
                details[position]
                    .hits
                    .push(self.string(get_u32(record, 8)?)?);
            }
        }
        let descriptor = self.index.descriptor(SECTION_TEST_DECISIONS)?;
        let vectors = self.index.descriptor(SECTION_TEST_VECTORS)?.count;
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_TEST_DECISIONS, index)?;
            if record[1..4].iter().any(|byte| *byte != 0)
                || record[12..16].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord("test decision record"));
            }
            if let Some(position) = position(record)? {
                let offset = get_u64(record, 16)?;
                let count = get_u64(record, 24)?;
                let end = offset
                    .checked_add(count)
                    .ok_or(CoverageIndexError::InvalidRecord("test vector range"))?;
                if end > vectors {
                    return Err(CoverageIndexError::InvalidRecord("test vector range"));
                }
                let mut observed = Vec::with_capacity(
                    usize::try_from(count).map_err(|_| CoverageIndexError::SizeOverflow)?,
                );
                for vector in offset..end {
                    observed.push(self.test_vector(vector)?);
                }
                details[position]
                    .decisions
                    .push(crate::coverage_report::TestDecisionResult {
                        id: self.string(get_u32(record, 8)?)?,
                        vectors: observed,
                    });
            }
        }
        Ok(details)
    }

    pub fn hit_metadata(
        &self,
        view: CoverageViewId,
    ) -> Result<Vec<IndexedHitMetadata>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_HIT_METADATA)?;
        let mut metadata = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_HIT_METADATA, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[2..4].iter().any(|byte| *byte != 0)
                || record[12..16].iter().any(|byte| *byte != 0)
                || record[44..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord("hit metadata record"));
            }
            let obligation = match record[1] {
                0 => "statement",
                1 => "function",
                2 => "branch",
                _ => return Err(CoverageIndexError::InvalidRecord("hit obligation")),
            };
            metadata.push(IndexedHitMetadata {
                id: self.string(get_u32(record, 4)?)?,
                obligation: obligation.into(),
                file: self.string(get_u32(record, 8)?)?,
                line: usize::try_from(get_u64(record, 16)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
                column: usize::try_from(get_u64(record, 24)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
                branch_kind: self.optional_string(get_u32(record, 32)?)?,
                label: self.optional_string(get_u32(record, 36)?)?,
                alternative: self.optional_string(get_u32(record, 40)?)?,
            });
        }
        Ok(metadata)
    }

    pub fn decision_metadata(
        &self,
        view: CoverageViewId,
    ) -> Result<Vec<crate::coverage_report::DecisionMeta>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_DECISION_METADATA)?;
        let mut metadata = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_DECISION_METADATA, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[1..4].iter().any(|byte| *byte != 0)
                || record[20..24].iter().any(|byte| *byte != 0)
                || record[56..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord(
                    "decision metadata record",
                ));
            }
            metadata.push(crate::coverage_report::DecisionMeta {
                id: self.string(get_u32(record, 4)?)?,
                file: self.string(get_u32(record, 8)?)?,
                source: self.string(get_u32(record, 12)?)?,
                kind: self.string(get_u32(record, 16)?)?,
                line: usize::try_from(get_u64(record, 24)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
                column: usize::try_from(get_u64(record, 32)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
                conditions: self.relation_strings(get_u64(record, 40)?, get_u64(record, 48)?)?,
            });
        }
        Ok(metadata)
    }

    pub fn phase_summaries(
        &self,
        view: CoverageViewId,
    ) -> Result<Vec<IndexedPhaseSummary>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_PHASE_SUMMARIES)?;
        let mut phases = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_PHASE_SUMMARIES, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[1..4].iter().any(|byte| *byte != 0)
                || record[48..].iter().any(|byte| *byte != 0)
            {
                return Err(CoverageIndexError::InvalidRecord("phase summary record"));
            }
            phases.push(IndexedPhaseSummary {
                id: self.string(get_u32(record, 4)?)?,
                kind: self.string(get_u32(record, 8)?)?,
                operation: self.string(get_u32(record, 12)?)?,
                source: self.optional_string(get_u32(record, 16)?)?,
                test: self.string(get_u32(record, 20)?)?,
                status: self.optional_string(get_u32(record, 24)?)?,
                caused_by_phase_id: self.optional_string(get_u32(record, 28)?)?,
                lines: usize::try_from(get_u64(record, 32)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
                decisions: usize::try_from(get_u64(record, 40)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
            });
        }
        Ok(phases)
    }

    pub fn anchors(
        &self,
        view: CoverageViewId,
        file: &str,
        line: usize,
    ) -> Result<Vec<IndexedAnchor>, CoverageIndexError> {
        let descriptor = self.index.descriptor(SECTION_ANCHORS)?;
        let mut anchors = Vec::new();
        for index in 0..descriptor.count {
            let record = self.index.record(SECTION_ANCHORS, index)?;
            if CoverageViewId::try_from(record[0])? != view {
                continue;
            }
            if record[3] != 0 || record[12..16].iter().any(|byte| *byte != 0) {
                return Err(CoverageIndexError::InvalidRecord("anchor record"));
            }
            let record_file = self.string(get_u32(record, 8)?)?;
            let record_line = usize::try_from(get_u64(record, 16)?)
                .map_err(|_| CoverageIndexError::SizeOverflow)?;
            if record_file != file || record_line != line {
                continue;
            }
            let total = usize::try_from(get_u64(record, 32)?)
                .map_err(|_| CoverageIndexError::SizeOverflow)?;
            let covered_conditions = usize::try_from(get_u64(record, 40)?)
                .map_err(|_| CoverageIndexError::SizeOverflow)?;
            let (kind, conditions, covered_conditions) = match record[1] {
                0 => {
                    if total == 0 || covered_conditions > total {
                        return Err(CoverageIndexError::InvalidRecord(
                            "decision anchor conditions",
                        ));
                    }
                    ("decision", Some(total), Some(covered_conditions))
                }
                1 => ("branch", None, None),
                2 => ("statement", None, None),
                3 => ("function", None, None),
                _ => return Err(CoverageIndexError::InvalidRecord("anchor kind")),
            };
            if kind != "decision" && (total != 0 || covered_conditions.is_some()) {
                return Err(CoverageIndexError::InvalidRecord("anchor conditions"));
            }
            anchors.push(IndexedAnchor {
                kind: kind.into(),
                id: self.string(get_u32(record, 4)?)?,
                file: record_file,
                line: record_line,
                column: usize::try_from(get_u64(record, 24)?)
                    .map_err(|_| CoverageIndexError::SizeOverflow)?,
                covered: bool_field(record[2])?,
                conditions,
                covered_conditions,
                tests: self.relation_strings(get_u64(record, 48)?, get_u64(record, 56)?)?,
            });
        }
        anchors.sort_by_key(|anchor| anchor.column);
        Ok(anchors)
    }

    pub fn snapshot(&self) -> Result<IndexedCoverageSnapshot, CoverageIndexError> {
        Ok(IndexedCoverageSnapshot {
            all_summary: self.summary(CoverageViewId::All)?,
            passed_summary: self.summary(CoverageViewId::Passed)?,
            failed_summary: self.summary(CoverageViewId::Failed)?,
            all_files: self.file_gaps(CoverageViewId::All, None, None)?,
            passed_files: self.file_gaps(CoverageViewId::Passed, None, None)?,
            failed_files: self.file_gaps(CoverageViewId::Failed, None, None)?,
        })
    }
}

fn bool_field(value: u8) -> Result<bool, CoverageIndexError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(CoverageIndexError::InvalidRecord("boolean")),
    }
}

fn decode_summary(
    record: &[u8],
    flags_offset: usize,
    base: usize,
) -> Result<CoverageSummary, CoverageIndexError> {
    let number = |offset: usize| -> Result<usize, CoverageIndexError> {
        usize::try_from(get_u64(record, offset)?).map_err(|_| CoverageIndexError::SizeOverflow)
    };
    let count = |offset: usize| -> Result<CoverageCount, CoverageIndexError> {
        let covered = number(offset)?;
        let total = number(offset + 8)?;
        if covered > total {
            return Err(CoverageIndexError::InvalidRecord("covered exceeds total"));
        }
        Ok(CoverageCount {
            covered,
            total,
            percentage: percentage(covered, total),
        })
    };
    let decisions = number(base)?;
    let executed_decisions = number(base + 8)?;
    let covered_decisions = number(base + 16)?;
    let conditions = number(base + 24)?;
    let covered_conditions = number(base + 32)?;
    if covered_decisions > executed_decisions
        || executed_decisions > decisions
        || covered_conditions > conditions
    {
        return Err(CoverageIndexError::InvalidRecord("summary count ordering"));
    }
    Ok(CoverageSummary {
        decisions,
        executed_decisions,
        covered_decisions,
        conditions,
        covered_conditions,
        condition_coverage_pct: percentage(covered_conditions, conditions),
        lines: count(base + 40)?,
        statements: count(base + 56)?,
        functions: count(base + 72)?,
        branches: count(base + 88)?,
        decision_outcomes: count(base + 104)?,
        condition_outcomes: count(base + 120)?,
        value_selections: count(base + 136)?,
        coverage_complete: bool_field(record[flags_offset])?,
        completeness_blocked: match record[flags_offset + 1] {
            0 => None,
            1 => Some(false),
            2 => Some(true),
            _ => return Err(CoverageIndexError::InvalidRecord("optional boolean")),
        },
    })
}

fn percentage(covered: usize, total: usize) -> f64 {
    if total == 0 {
        100.0
    } else {
        ((covered as f64 / total as f64) * 10_000.0).round() / 100.0
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{
        coverage_analysis::{McdcVector, PointKind},
        coverage_report::{
            CoverageManifest, CoverageReportRequest, DecisionMeta, ExitCodeInput, PointMeta,
            RawTestResult, RuntimeSnapshot, TestProvenance, analyze_coverage_results,
        },
        query_index::{QueryIndexIdentity, write_query_index},
    };

    use super::*;

    fn root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-coverage-index-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn identity() -> QueryIndexIdentity {
        QueryIndexIdentity {
            evidence_sha256: [1; 32],
            evidence_bytes: 100,
            analysis_sha256: [2; 32],
            producer_sha256: [3; 32],
            archive_schema_version: 2,
        }
    }

    fn report() -> CoverageReport {
        let decision = DecisionMeta {
            id: "d".into(),
            file: "src/a.js".into(),
            line: 1,
            column: 1,
            source: "a && b".into(),
            conditions: vec!["a".into(), "b".into()],
            kind: "if".into(),
        };
        analyze_coverage_results(&CoverageReportRequest {
            run_id: "run".into(),
            manifest: CoverageManifest {
                decisions: vec![decision.clone()],
                points: vec![PointMeta {
                    id: "point".into(),
                    kind: PointKind::Statement,
                    file: "src/a.js".into(),
                    line: 2,
                    column: 3,
                    source: "work();".into(),
                    label: None,
                }],
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
            },
            raw_results: vec![RawTestResult {
                test_id: Some("test".into()),
                scope: None,
                test: "test".into(),
                test_file: Some("tests/a.js".into()),
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
                },
                role: "test".into(),
                phases: Vec::new(),
                runtime: vec![RuntimeSnapshot {
                    decisions: vec![crate::coverage_report::DecisionSnapshot {
                        meta: decision,
                        vectors: vec![McdcVector {
                            values: vec![Some(false), None],
                            outcome: false,
                        }],
                    }],
                    hits: vec!["point".into()],
                    events: Vec::new(),
                }],
                browser: Vec::new(),
                server: Vec::new(),
            }],
            generated_at: "time".into(),
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(0)),
        })
        .unwrap()
    }

    #[test]
    fn typed_columns_round_trip_all_outcome_views_without_json() {
        let report = report();
        let root = root();
        let path = root.join("query-index.v1.bin");
        write_query_index(
            &coverage_index_sections(&report).unwrap(),
            &identity(),
            &path,
        )
        .unwrap();
        let container = QueryIndex::open(&path, &identity()).unwrap();
        let index = CoverageIndex::new(&container).unwrap();
        for (id, view) in [
            (CoverageViewId::All, &report.view),
            (CoverageViewId::Passed, &report.filters.passed),
            (CoverageViewId::Failed, &report.filters.failed),
        ] {
            assert_eq!(index.summary(id).unwrap(), view.summary);
        }
        let gaps = index.file_gaps(CoverageViewId::All, None, None).unwrap();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].file, "src/a.js");
        assert_eq!(gaps[0].missing_mcdc_conditions, 2);
        let projection = index.projection(CoverageViewId::All, None, None).unwrap();
        assert_eq!(projection.summary, report.view.summary);
        assert_eq!(projection.tests, 1);
        assert_eq!(projection.setups, 0);
        assert_eq!(projection.test_outcomes.passed, 1);
        assert!(projection.source_scope.is_none());
        let line = index
            .line(CoverageViewId::All, "src/a.js", 2)
            .unwrap()
            .unwrap();
        assert!(line.covered);
        assert_eq!(line.tests, ["test"]);
        assert_eq!(line.confidence.level, "executed");
        let tests = index.test_summaries(CoverageViewId::All).unwrap();
        assert_eq!(tests.len(), 1);
        assert_eq!(tests[0].provenance.runner, "node:test");
        let decision = index.anchors(CoverageViewId::All, "src/a.js", 1).unwrap();
        assert_eq!(decision.len(), 1);
        assert_eq!(decision[0].kind, "decision");
        assert_eq!(decision[0].conditions, Some(2));
        assert_eq!(decision[0].tests, ["test"]);
        let point = index.anchors(CoverageViewId::All, "src/a.js", 2).unwrap();
        assert_eq!(point.len(), 1);
        assert_eq!(point[0].kind, "statement");
        assert_eq!(point[0].tests, ["test"]);
        let details = index.test_details(CoverageViewId::All).unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].retries, [0]);
        assert_eq!(details[0].attempts.len(), 1);
        assert_eq!(details[0].hits, ["point"]);
        assert_eq!(details[0].lines.len(), 1);
        assert_eq!(details[0].lines[0].line, 2);
        assert_eq!(details[0].decisions.len(), 1);
        assert_eq!(details[0].decisions[0].vectors.len(), 1);
        assert_eq!(
            details[0].decisions[0].vectors[0].values,
            [Some(false), None]
        );
        let hits = index.hit_metadata(CoverageViewId::All).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, "point");
        let decisions = index.decision_metadata(CoverageViewId::All).unwrap();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].conditions, ["a", "b"]);
        fs::remove_dir_all(root).unwrap();
    }
}
