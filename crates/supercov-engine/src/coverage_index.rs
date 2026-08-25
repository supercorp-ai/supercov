//! Typed coverage columns stored in the immutable query-index container.
//!
//! This is not a serialized report. Records contain fixed-width values and
//! checked references into an interned UTF-8 string table. New query surfaces
//! add sections without forcing existing readers to parse unrelated data.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;

use crate::{
    coverage_analysis::{CoverageCount, CoverageSummary, find_witnesses_for_conditions},
    coverage_report::{CoverageReport, CoverageView},
    query_index::{QueryIndex, QueryIndexError, QueryIndexSection},
};

pub const SECTION_STRING_BYTES: u32 = 1;
pub const SECTION_STRINGS: u32 = 2;
pub const SECTION_VIEW_SUMMARIES: u32 = 10;
pub const SECTION_FILE_GAPS: u32 = 11;
pub const SECTION_DECISION_GAPS: u32 = 12;

const STRING_RECORD_SIZE: usize = 16;
const SUMMARY_RECORD_SIZE: usize = 176;
const FILE_GAP_RECORD_SIZE: usize = 176;
const DECISION_GAP_RECORD_SIZE: usize = 96;
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

pub fn coverage_index_sections(
    report: &CoverageReport,
) -> Result<Vec<QueryIndexSection>, CoverageIndexError> {
    let views = [
        (CoverageViewId::All, &report.view),
        (CoverageViewId::Passed, &report.filters.passed),
        (CoverageViewId::Failed, &report.filters.failed),
    ];
    let mut strings = StringTable::default();
    let mut summaries = Vec::with_capacity(views.len() * SUMMARY_RECORD_SIZE);
    let mut gaps = Vec::new();
    let mut decision_gaps = Vec::new();
    for (id, view) in views {
        summaries.extend_from_slice(&summary_record(id, view, &mut strings)?);
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
    Ok(vec![
        blob,
        string_records,
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
        ] {
            if index.descriptor(kind)?.record_size as usize != size {
                return Err(CoverageIndexError::InvalidRecord("record size"));
            }
        }
        index.descriptor(SECTION_STRING_BYTES)?;
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
        coverage_analysis::McdcVector,
        coverage_report::{
            CoverageManifest, CoverageReportRequest, DecisionMeta, ExitCodeInput, RawTestResult,
            RuntimeSnapshot, TestProvenance, analyze_coverage_results,
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
                points: Vec::new(),
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
                    hits: Vec::new(),
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
        fs::remove_dir_all(root).unwrap();
    }
}
