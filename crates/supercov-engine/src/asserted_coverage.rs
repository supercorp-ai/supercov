//! The language-neutral join of asserted coverage: sites, evidence and observations in, a verdict per
//! site out.
//!
//! Frontends contribute facts and never verdicts, so everything syntactic happens before this module:
//! which boundaries a site's effect or value reaches, which sites each outcome of a decision controls,
//! what each assertion reads and how strongly. This module decides only what those facts imply, which is
//! the one part of the metric that is the same in every language.
//!
//! **Evident** and **presence** are candidate classifications under the frontend's flow and observation
//! rules, not formal proofs of arbitrary-change detection. Unresolved sites carry a reason: a *gap*
//! a test can close, or an *analysis limit* the frontend could not follow. Limits remain visible and
//! must not be silently removed from a reported accuracy denominator.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Strength
// ---------------------------------------------------------------------------

/// How strongly the modeled assertion checks what it reads. This classifies its predicate, not a
/// formal proof of the complete source-to-assertion dependency; `Presence` says a value arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strength {
    Presence,
    Value,
    Total,
}

impl Strength {
    /// Does this strength distinguish one value from another?
    fn caught(self) -> bool {
        self >= Strength::Value
    }
}

fn stronger(a: Option<Strength>, b: Option<Strength>) -> Option<Strength> {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some(x), Some(y)) => Some(x.max(y)),
    }
}

// ---------------------------------------------------------------------------
// Facts in
// ---------------------------------------------------------------------------

/// A place a site's effect or value arrives, and which an assertion may read: a function's return or
/// escape, a sink the test injected, a mocked module, an installed global, a process channel, rendered
/// output, or `internal` for an effect that leaves no boundary at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Boundary {
    pub boundary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub facet: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub via: Option<String>,
}

impl Boundary {
    fn internal(&self) -> bool {
        self.boundary == "internal"
    }
}

/// Which part of a mock's history a passing assertion reads. This is source
/// evidence; relating a history element to a production site remains unresolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockProjection {
    pub target: String,
    pub kind: String,
    pub path: Vec<String>,
    /// A bounded source-model count, not argument protection or general site credit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_evidence: Option<MockCountEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockCountCall {
    pub source: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub site: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockCountEvidence {
    pub model: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed_at_read: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_count: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub calls: Option<Vec<MockCountCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_selections: Option<Vec<MockHistorySelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_binding: Option<MockCountRowBinding>,
}

/// A bounded selection from a copied history, not a claim about the whole mock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockHistorySelection {
    pub source: String,
    pub input_count: u64,
    pub from: u64,
    pub to: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockCountRowValue {
    pub declaration: String,
    pub name: String,
    pub value: serde_json::Value,
}

/// Source-reproduced registration inputs; not a value-protection proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MockCountRowBinding {
    pub model: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#loop: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub table: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub row_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bindings: Option<Vec<MockCountRowValue>>,
}

/// Source identities of both operands. Equal source text is not binding identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonOperand {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<SourcePrimitive>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<ComparisonInput>,
}

/// Source literals, not sampled runtime values. Numeric text preserves -0.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "lowercase")]
pub enum SourcePrimitive {
    Number(String),
    String(String),
    Boolean(bool),
    Null,
}

impl SourcePrimitive {
    fn same_value(&self, other: &Self) -> Option<bool> {
        let valid = |v: &Self| match v {
            Self::Number(n) => n.parse::<f64>().ok().is_some_and(f64::is_finite),
            _ => true,
        };
        if !valid(self) || !valid(other) {
            return None;
        }
        Some(match (self, other) {
            (Self::Number(a), Self::Number(b)) => {
                a.parse::<f64>().ok()?.to_bits() == b.parse::<f64>().ok()?.to_bits()
            }
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Boolean(a), Self::Boolean(b)) => a == b,
            (Self::Null, Self::Null) => true,
            _ => false,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimitiveDecisionCheck {
    pub test: String,
    pub assertion_source: String,
    pub predicate: String,
    pub expected: SourcePrimitive,
    pub original_outcome: bool,
}

/// Bounded direct-call, side-effect-free literal branches checked by the frontend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PrimitiveDecision {
    pub model: String,
    pub source: String,
    pub when_true: SourcePrimitive,
    pub when_false: SourcePrimitive,
    pub checks: Vec<PrimitiveDecisionCheck>,
}

/// Input provenance through const aliases/await, not evaluated value identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComparisonInput {
    pub binding: String,
    pub awaits: Vec<String>,
}

/// Checked resolver syntax, not a checked runtime/producer instance relation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExitEvidence {
    pub model: String,
    pub status: String,
    pub reason: String,
    pub operand: String,
    pub helper_calls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promise: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spawn: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<ProcessExitEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<ProcessExitResolution>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumer: Option<ProcessExitConsumer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExitConsumer {
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub bindings: Vec<String>,
    pub read: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessExitEvent {
    pub source: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessExitResolution {
    pub status: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    pub event_argument: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Comparison {
    pub predicate: String,
    pub actual: ComparisonOperand,
    pub expected: ComparisonOperand,
    pub relation: String,
}

/// Predicate strength applies to the projected value, not automatically to the
/// arguments or count of each production call contributing to a mock history.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub boundary: String,
    #[serde(default)]
    pub facet: Option<String>,
    pub strength: Strength,
    #[serde(default, rename = "where")]
    pub where_: Option<String>,
    /// Exact authored call identity, retained for assertion-specific hint checks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion_source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assertion_method: Option<String>,
    /// `expect(x).not.toHaveBeenCalled()`: the assertion pins that something did *not* happen
    #[serde(default)]
    pub negative: bool,
    /// the assertion pins a sink's whole call list, so a spurious or missing call shows up
    #[serde(default)]
    pub call_list: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock: Option<MockProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison: Option<Comparison>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_exit: Option<ProcessExitEvidence>,
    /// a whole-page render witnessed the site: it ran, but its output was not read
    #[serde(default)]
    pub weak: bool,
    /// log sites whose message this observation's pattern, literal or fragment admits; `None` when the
    /// observation carries no message constraint and so admits every site on its channel
    #[serde(default)]
    pub log_sites: Option<Vec<String>>,
    /// the observation matches on a regex that several log sites can satisfy, so it pins none of them
    /// individually; a whole literal or a fragment does not carry that ambiguity
    #[serde(default)]
    pub pattern_shared: bool,
}

impl Observation {
    /// Reflexive comparisons do not constrain their stable value. Shared input
    /// through await may constrain something, but needs a separate dependence
    /// model before it can supply producer value, absence or pragma credit.
    /// Exit channels additionally need a checked producer/result-instance link;
    /// even accepted resolver source syntax is insufficient on its own.
    fn can_constrain_value(&self) -> bool {
        self.mock.is_none()
            && self.boundary != "exit"
            && self.comparison.as_ref().is_none_or(|c| {
                !matches!(
                    c.relation.as_str(),
                    "same-immutable-binding" | "shared-input-through-await"
                )
            })
    }

    /// Does this observation read the given boundary of the given site? Dense channels need the message
    /// constraint checked, and a spy on one console method sees only that method's calls.
    fn matches(&self, site: &Site, at: &Boundary) -> bool {
        if !self.can_constrain_value() {
            return false;
        }
        if at.boundary != self.boundary {
            return false;
        }
        match at.boundary.as_str() {
            "client-header" => match (at.facet.as_deref(), self.facet.as_deref()) {
                (Some("*"), _) | (_, None) | (_, Some("*")) => true,
                (a, b) => a == b,
            },
            "stderr" | "stdout" => {
                if let (Some(facet), true) = (self.facet.as_deref(), site.category == "log")
                    && let Some(method) = facet.strip_prefix("console.")
                    && site.method.as_deref().is_some_and(|m| m != method)
                {
                    return false;
                }
                match &self.log_sites {
                    // a message constraint pins only the sites whose template can produce it
                    Some(admitted) => {
                        if site.category == "log" {
                            admitted.contains(&site.id)
                        } else {
                            at.facet.as_deref() != Some("log")
                        }
                    }
                    None => true,
                }
            }
            "client-message" => match at.facet.as_deref() {
                Some(facet) => self.facet.as_deref().is_some_and(|f| f.contains(facet)),
                None => true,
            },
            _ => true,
        }
    }
}

/// A test-owned object the test passed into production, and the parameter it arrived through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SinkBinding {
    pub sink: String,
    pub param: String,
    #[serde(default)]
    pub member: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestFacts {
    pub id: String,
    pub file: String,
    pub observations: Vec<Observation>,
    #[serde(default)]
    pub sinks: Vec<SinkBinding>,
    /// components the test rendered, by owner name
    #[serde(default)]
    pub rendered: Vec<String>,
    /// Rejected/unavailable witnesses are not observations. Legacy frontends
    /// omit this field; the public JS adapter requires the versioned capability.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub witness_issues: Vec<WitnessIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WitnessIssueKind {
    CaptureUnavailable,
    /// Runtime attribution exists, but no source test body could be linked.
    /// This is missing analysis, not evidence that the test contains no oracle.
    TestSourceUnlinked,
    CallNotRecorded,
    CallIncomplete,
    MixedCallOutcomes,
    CallFailed,
    UninstrumentedObservation,
}

impl WitnessIssueKind {
    fn applies_to_whole_test(self) -> bool {
        matches!(self, Self::CaptureUnavailable | Self::TestSourceUnlinked)
    }

    fn is_uncertain(self) -> bool {
        self != Self::CallFailed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WitnessIssue {
    pub kind: WitnessIssueKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    /// A statically recognized but rejected observation. Used only to bound
    /// uncertainty, NEVER to supply strength or evidence to a resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observation: Option<Observation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestWitnessIssue {
    pub test: String,
    #[serde(flatten)]
    pub issue: WitnessIssue,
}

/// Deserialize a field whose absence and whose explicit `null` mean different things: the field's own
/// `Option` distinguishes "the key was there" from "it was not", so a present `null` becomes `Some(None)`
/// instead of collapsing into the same `None` a missing key gives.
fn present_option<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: serde::Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// The sites each outcome of a decision controls, and the site sets its witness rules ask about.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive: Option<PrimitiveDecision>,
    /// the site that carries the decision's value (a ternary inside a return, say)
    #[serde(default)]
    pub carrier: Option<String>,
    /// a value-position expression not inside a site: the sites its value flows to
    #[serde(default)]
    pub value_flow: Option<Vec<String>>,
    /// sites the true outcome controls
    #[serde(default)]
    pub then: Option<Vec<String>>,
    /// Sites the false outcome controls. Three states, and they mean different things: absent when the
    /// decision has no branches at all (a value-position operand), `Some(None)` when there is no else
    /// branch (the absence case, where a spurious effect is what a test would notice), and `Some(sites)`
    /// for a real else. An explicit JSON `null` has to survive as `Some(None)`, which is why this reads
    /// the field itself rather than letting a missing key and a null one collapse together.
    #[serde(default, rename = "else", deserialize_with = "present_option")]
    pub else_: Option<Option<Vec<String>>>,
    #[serde(default)]
    pub early_exit_downstream: Option<Vec<String>>,
    #[serde(default)]
    pub loop_body: Option<Vec<String>>,
    #[serde(default)]
    pub default_kept: Option<Vec<DefaultKept>>,
    #[serde(default)]
    pub object_valued: Option<ObjectValued>,
    /// tests that took each outcome; absent when the frontend has no per-test outcome data
    #[serde(default)]
    pub outcomes: Option<Outcomes>,
    /// tests in which a value-position operand was the selected one
    #[serde(default)]
    pub selected: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DefaultKept {
    pub write: String,
    pub dependents: Vec<Dependent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Dependent {
    pub site: String,
    pub label: String,
    #[serde(default)]
    pub strength: Option<Strength>,
    /// Candidate timer-cancellation dependency: active only when a callback site has total evidence.
    /// Older prototype facts already filtered these candidates and omit this condition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires_total: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectValued {
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Outcomes {
    #[serde(rename = "true")]
    pub true_: Vec<String>,
    #[serde(rename = "false")]
    pub false_: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Site {
    pub id: String,
    pub file: String,
    pub line: u32,
    pub kind: String,
    pub category: String,
    pub classification: String,
    pub owner: String,
    #[serde(default)]
    pub method: Option<String>,
    pub bounds: Vec<Boundary>,
    /// The site's own boundaries, without the flow that carries its value elsewhere. What another site's
    /// value reaches here is read at these, since the carrying flow does not continue past this point.
    #[serde(default)]
    pub direct_bounds: Vec<Boundary>,
    /// sites this site's value flows into
    #[serde(default)]
    pub reached: Vec<String>,
    pub covered_by: Vec<String>,
    #[serde(default)]
    pub object_valued_return: bool,
    /// operand shapes a covering test asserted that the frontend could not trace to a boundary
    #[serde(default)]
    pub unmodelled_shapes: Vec<String>,
    #[serde(default)]
    pub decision: Option<DecisionFacts>,
    /// sites through which an internal effect becomes observable
    #[serde(default)]
    pub derive: Vec<Dependent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Facts {
    pub schema: u32,
    pub sites: Vec<Site>,
    pub tests: Vec<TestFacts>,
    /// `vi.mock` boundaries, which depend on the test file rather than the test: file → site → boundaries
    #[serde(default)]
    pub mocks_by_test_file: BTreeMap<String, BTreeMap<String, Vec<Boundary>>>,
}

/// A source suggestion, deliberately separate from observations and join inputs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PragmaHint {
    pub id: String,
    #[serde(rename = "where")]
    pub where_: String,
    pub raw: String,
    pub target: Option<PragmaTarget>,
    pub candidate_sites: Vec<String>,
    pub issue: Option<String>,
    pub test: Option<String>,
    pub assertion_source: Option<String>,
    pub assertion_method: Option<String>,
    pub witness: String,
    pub witness_issue: Option<String>,
    /// A checked source pattern, not a captured read or a passing assertion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaited_observation: Option<AwaitedObservationSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub check: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub call_omission: Option<CallOmissionEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_sensitivity: Option<CountSensitivityEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload_sensitivity: Option<PayloadSensitivityEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct_return_sensitivity: Option<DirectReturnSensitivityEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectReturnSensitivityEvidence {
    pub model: String,
    pub status: String,
    pub reason: Option<String>,
    pub scope: Option<String>,
    pub assertion_source: Option<String>,
    pub target_source: Option<String>,
    pub change_source: Option<String>,
    pub change_text: Option<String>,
    pub original: Option<DirectReturnCheck>,
    pub variants: Option<Vec<DirectReturnVariant>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectReturnCheck {
    pub predicate: String,
    pub actual: serde_json::Value,
    pub expected: serde_json::Value,
    pub call_source: String,
    pub target_evaluations: u64,
    pub outcome: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectReturnVariant {
    pub change: String,
    pub status: String,
    pub reason: Option<String>,
    pub check: Option<DirectReturnCheck>,
}

fn direct_return_evidence_issue(
    hint: &PragmaHint,
    e: &DirectReturnSensitivityEvidence,
    site: &Site,
    test: &TestFacts,
) -> Option<String> {
    let test_file = test.file.as_str();
    let location_in = |location: &str, file: &str| {
        location
            .strip_prefix(&format!("{file}:"))
            .and_then(|rest| rest.split_once(':'))
            .is_some_and(|(line, column)| {
                line.parse::<u32>().is_ok_and(|n| n > 0)
                    && column.parse::<u32>().is_ok_and(|n| n > 0)
            })
    };
    if e.model != "node-first-test-direct-return-v1"
        || e.status != "source-checked"
        || e.reason.is_some()
        || e.scope.as_deref() != Some("first-synchronous-test-prefix")
        || e.assertion_source != hint.assertion_source
        || e.assertion_source
            .as_ref()
            .is_none_or(|s| !location_in(s, test_file))
        || e.target_source
            .as_ref()
            .is_none_or(|s| !location_in(s, &site.file))
        || e.target_source
            .as_ref()
            .is_none_or(|s| !s.starts_with(&format!("{}:{}:", site.file, site.line)))
        || e.change_source
            .as_ref()
            .is_none_or(|s| !location_in(s, &site.file))
        || e.change_text.as_ref().is_none_or(String::is_empty)
        || !(site.kind == "decision" || site.category == "return")
        || hint.payload_sensitivity.is_some()
        || test.witness_issues.iter().any(|issue| {
            issue.kind.applies_to_whole_test()
                || (issue.source.is_some() && issue.source == hint.assertion_source)
        })
    {
        return Some(
            e.reason
                .clone()
                .unwrap_or_else(|| "unsupported-direct-return-evidence".into()),
        );
    }
    let (Some(original), Some(variants)) = (&e.original, &e.variants) else {
        return Some("incomplete-direct-return-evidence".into());
    };
    // This model has no opaque strings or assumed original predicate results.
    fn concrete(value: &serde_json::Value, depth: usize) -> bool {
        if depth > 32 {
            return false;
        }
        match value["kind"].as_str() {
            Some("undefined" | "null" | "string" | "number" | "boolean") => true,
            Some("array" | "object") => value["properties"]
                .as_array()
                .is_some_and(|ps| ps.iter().all(|p| concrete(&p["value"], depth + 1))),
            _ => false,
        }
    }
    let valid = |c: &DirectReturnCheck| {
        let mut budget = 4096;
        matches!(
            c.predicate.as_str(),
            "node-same-value" | "node-deep-strict-equality"
        ) && location_in(&c.call_source, test_file)
            && (1..=4096).contains(&c.target_evaluations)
            && valid_payload(&c.actual, 0, &mut budget)
            && valid_payload(&c.expected, 0, &mut budget)
            && concrete(&c.actual, 0)
            && concrete(&c.expected, 0)
            && payload_equal(
                &c.actual,
                &c.expected,
                c.predicate == "node-deep-strict-equality",
            )
            .is_some_and(|equal| c.outcome == if equal { "not-rejected" } else { "rejected" })
    };
    if !valid(original)
        || original.outcome != "not-rejected"
        || !matches!(
            (
                hint.assertion_method.as_deref(),
                original.predicate.as_str()
            ),
            (Some("equal" | "strictEqual"), "node-same-value")
                | (
                    Some("deepEqual" | "deepStrictEqual"),
                    "node-deep-strict-equality"
                )
        )
    {
        return Some("direct-return-assertion-not-modelled".into());
    }
    let mut names: Vec<_> = variants.iter().map(|v| v.change.as_str()).collect();
    names.sort();
    if !(names == ["boolean-literal-inverted"]
        || names == ["condition-false", "condition-inverted", "condition-true"])
        || (names == ["boolean-literal-inverted"]
            && !matches!(e.change_text.as_deref(), Some("true" | "false")))
        || variants.iter().any(|v| match v.status.as_str() {
            "source-checked" => {
                v.reason.is_some()
                    || v.check.as_ref().is_none_or(|c| {
                        !valid(c)
                            || c.call_source != original.call_source
                            || c.expected != original.expected
                            || c.predicate != original.predicate
                    })
            }
            "unresolved" => v.reason.as_ref().is_none_or(String::is_empty) || v.check.is_some(),
            _ => true,
        })
    {
        return Some("inconsistent-direct-return-variants".into());
    }
    if variants.iter().all(|v| v.status != "source-checked") {
        return Some("direct-return-variants-unavailable".into());
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadSensitivityEvidence {
    pub model: String,
    pub status: String,
    pub reason: Option<String>,
    pub scope: Option<String>,
    pub assertion_source: Option<String>,
    pub target_source: Option<String>,
    pub change_source: Option<String>,
    pub change_text: Option<String>,
    pub allocations: Option<Vec<String>>,
    pub original: Option<PayloadCheck>,
    pub variants: Option<Vec<PayloadVariant>>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadCheck {
    pub predicate: String,
    pub actual: serde_json::Value,
    pub expected: serde_json::Value,
    pub projection: PayloadProjection,
    pub outcome: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayloadProjection {
    pub instance: String,
    pub call_source: String,
    pub call_index: u64,
    pub argument_index: u64,
    pub read_at: String,
    pub history_selections: Option<Vec<MockHistorySelection>>,
    pub coercion: Option<PayloadCoercion>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadCoercion {
    pub source: String,
    pub rule: String,
    pub input: serde_json::Value,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PayloadVariant {
    pub change: String,
    pub status: String,
    pub reason: Option<String>,
    pub check: Option<PayloadCheck>,
}

fn valid_payload(v: &serde_json::Value, depth: usize, budget: &mut usize) -> bool {
    if depth > 32 || *budget == 0 {
        return false;
    }
    *budget -= 1;
    let Some(o) = v.as_object() else {
        return false;
    };
    match v["kind"].as_str() {
        Some("undefined" | "null" | "opaque-string") => o.len() == 1,
        Some("string") => o.len() == 2 && v["value"].is_string(),
        Some("quoted-string") => {
            o.len() == 2
                && v["value"].as_str().is_some_and(|s| {
                    s.len() <= 64
                        && s.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b" _-".contains(&c))
                })
        }
        Some("substring-pattern") => {
            o.len() == 2
                && v["value"].as_str().is_some_and(|s| {
                    !s.is_empty()
                        && s.len() <= 80
                        && s.bytes()
                            .all(|c| c.is_ascii_alphanumeric() || b" _:-".contains(&c))
                })
        }
        Some("boolean") => o.len() == 2 && v["value"].is_boolean(),
        Some("number") => {
            o.len() == 2
                && v["value"]
                    .as_f64()
                    .is_some_and(|n| n.is_finite() && !(n == 0.0 && n.is_sign_negative()))
        }
        Some(kind @ ("object" | "array")) => {
            let Some(properties) = v["properties"].as_array() else {
                return false;
            };
            let mut names = std::collections::HashSet::new();
            o.len() == 2
                && properties.iter().all(|p| {
                    p.as_object().is_some_and(|o| o.len() == 2)
                        && p["name"]
                            .as_str()
                            .is_some_and(|n| n != "__proto__" && names.insert(n))
                        && valid_payload(&p["value"], depth + 1, budget)
                })
                && (kind != "array"
                    || (0..properties.len()).all(|i| names.contains(i.to_string().as_str())))
        }
        _ => false,
    }
}

// Both values have already passed bounded shape validation. Unknown string bytes
// must never compare equal just because their abstract descriptions are equal.
fn payload_equal(a: &serde_json::Value, b: &serde_json::Value, deep: bool) -> Option<bool> {
    let (ak, bk) = (a["kind"].as_str()?, b["kind"].as_str()?);
    if ak == "quoted-string" || bk == "quoted-string" {
        let other = if ak == "quoted-string" { b } else { a };
        return match other["kind"].as_str()? {
            "string" => {
                if other["value"].as_str()?.contains('\'') {
                    None
                } else {
                    Some(false)
                }
            }
            "quoted-string" | "opaque-string" => None,
            _ => Some(false),
        };
    }
    if ak == "opaque-string" || bk == "opaque-string" {
        let other = if ak == "opaque-string" { bk } else { ak };
        return if matches!(other, "string" | "opaque-string") {
            None
        } else {
            Some(false)
        };
    }
    if ak != bk {
        return Some(false);
    }
    if matches!(ak, "object" | "array") {
        if !deep {
            return None;
        }
        let (ap, bp) = (a["properties"].as_array()?, b["properties"].as_array()?);
        if ap.len() != bp.len() {
            return Some(false);
        }
        let mut unknown = false;
        for p in ap {
            let Some(q) = bp.iter().find(|q| q["name"] == p["name"]) else {
                return Some(false);
            };
            match payload_equal(&p["value"], &q["value"], true) {
                Some(false) => return Some(false),
                None => unknown = true,
                _ => (),
            }
        }
        return if unknown { None } else { Some(true) };
    }
    if ak == "number" {
        return Some(a["value"].as_f64()? == b["value"].as_f64()?);
    }
    Some(a["value"] == b["value"])
}

fn payload_predicate(c: &PayloadCheck) -> Option<bool> {
    if c.predicate == "node-literal-regexp" {
        if c.expected["kind"] != "substring-pattern" || c.actual["kind"] != "string" {
            return None;
        }
        return Some(
            c.actual["value"]
                .as_str()?
                .contains(c.expected["value"].as_str()?),
        );
    }
    payload_equal(
        &c.actual,
        &c.expected,
        c.predicate == "node-deep-strict-equality",
    )
}

fn valid_payload_coercion(c: &PayloadCheck) -> bool {
    let Some(coercion) = &c.projection.coercion else {
        return true;
    };
    if coercion.source != c.projection.read_at || !valid_payload(&coercion.input, 0, &mut 4096) {
        return false;
    }
    match coercion.rule.as_str() {
        "string-identity" => {
            matches!(
                coercion.input["kind"].as_str(),
                Some("string" | "opaque-string" | "quoted-string")
            ) && c.actual == coercion.input
        }
        "plain-object-default-string" => {
            coercion.input["kind"] == "object"
                && coercion.input["properties"].as_array().is_some_and(|p| {
                    p.iter()
                        .all(|p| !matches!(p["name"].as_str(), Some("toString" | "valueOf")))
                })
                && c.actual == serde_json::json!({"kind":"string","value":"[object Object]"})
        }
        _ => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CountSensitivityEvidence {
    pub model: String,
    pub status: String,
    pub reason: Option<String>,
    pub scope: Option<String>,
    pub assertion_source: Option<String>,
    pub target_source: Option<String>,
    pub condition_source: Option<String>,
    pub condition_text: Option<String>,
    pub allocations: Option<Vec<String>>,
    pub instance: Option<String>,
    pub expected_count: Option<u64>,
    pub original_count: Option<u64>,
    pub variants: Option<Vec<CountSensitivityVariant>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CountSensitivityVariant {
    pub change: String,
    pub status: String,
    pub reason: Option<String>,
    pub count: Option<u64>,
    pub outcome: Option<String>,
}

/// A bounded check of one specified edit, never general value or site credit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CallOmissionEvidence {
    pub model: String,
    pub status: String,
    pub reason: Option<String>,
    pub outcome: Option<String>,
    pub scope: Option<String>,
    pub assertion_source: Option<String>,
    pub call_source: Option<String>,
    pub callback_source: Option<String>,
    pub instance: Option<String>,
    pub expected_count: Option<u64>,
    pub original_count: Option<u64>,
    pub omitted_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AwaitedObservationSource {
    pub model: String,
    pub factory_source: String,
    pub predicate_source: String,
    pub captures: Vec<ObservationCaptureSource>,
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObservationCaptureSource {
    pub stream: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PragmaTarget {
    pub file: String,
    pub function: String,
    pub snippet: Option<String>,
    /// Explanatory text only. Never interpreted as a verified dependency.
    pub via: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HintValidation {
    AnalyzerSupported,
    Unresolved,
    Invalid,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PragmaCheck {
    pub hint: PragmaHint,
    pub origin: String,
    pub validation: HintValidation,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strength: Option<Strength>,
    /// Only the selected assertion's observations, never another assertion in its test.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub observations: Vec<Observation>,
}

/// Check source hints against existing facts, without modifying the normal join.
/// Indexes are shared across hints; only the selected test's observations are copied.
/// This bounded first pass supports effect boundaries/flow, not decision or internal
/// derivation proofs. Unsupported paths remain unresolved, not contradicted.
pub fn check_pragma_hints(facts: &Facts, hints: &[PragmaHint]) -> Vec<PragmaCheck> {
    if hints.is_empty() {
        return vec![];
    }
    let engine = Join {
        sites: facts.sites.iter().map(|s| (s.id.as_str(), s)).collect(),
        tests: facts.tests.iter().map(|t| (t.id.as_str(), t)).collect(),
        facts,
        resolved: BTreeMap::new(),
    };
    let mut assertions: BTreeMap<(&str, &str, &str), Vec<&Observation>> = BTreeMap::new();
    for test in &facts.tests {
        for ob in &test.observations {
            if let (Some(source), Some(method)) = (&ob.assertion_source, &ob.assertion_method)
                && ob.boundary != "pragma"
            {
                assertions
                    .entry((&test.id, source, method))
                    .or_default()
                    .push(ob);
            }
        }
    }
    hints
        .iter()
        .map(|hint| {
            let mut result = PragmaCheck {
                hint: hint.clone(),
                origin: "user-suggested".into(),
                validation: HintValidation::Unresolved,
                reason: "connection-not-established".into(),
                strength: None,
                observations: vec![],
            };
            if let Some(issue) = &hint.issue {
                result.reason = issue.clone();
                // Missing inventory can mean an unsupported source construct,
                // not a bad declaration. Do not call an analysis limit invalid.
                if !matches!(
                    issue.as_str(),
                    "no-owning-passed-test" | "target-not-in-inventory"
                ) {
                    result.validation = HintValidation::Invalid;
                }
                return result;
            }
            let [id] = hint.candidate_sites.as_slice() else {
                result.validation = HintValidation::Invalid;
                result.reason = "target-not-unique".into();
                return result;
            };
            let Some(site) = engine.sites.get(id.as_str()) else {
                result.validation = HintValidation::Invalid;
                result.reason = "target-not-found".into();
                return result;
            };
            if hint
                .target
                .as_ref()
                .is_none_or(|target| target.file != site.file)
            {
                result.validation = HintValidation::Invalid;
                result.reason = "target-file-mismatch".into();
                return result;
            }
            if hint.awaited_observation.is_some() {
                result.reason = "observation-capture-unavailable".into();
                return result;
            }
            if hint.witness != "passed" || hint.witness_issue.is_some() {
                result.reason = hint
                    .witness_issue
                    .clone()
                    .unwrap_or_else(|| "missing-passed-witness".into());
                return result;
            }
            let Some(test) = hint
                .test
                .as_ref()
                .and_then(|id| engine.tests.get(id.as_str()))
            else {
                result.reason = "no-owning-passed-test".into();
                return result;
            };
            if test
                .witness_issues
                .iter()
                .any(|issue| issue.kind == WitnessIssueKind::TestSourceUnlinked)
            {
                result.reason = "test-source-unlinked".into();
                return result;
            }
            if hint.assertion_source.as_ref().is_none_or(String::is_empty)
                || hint.assertion_method.as_ref().is_none_or(String::is_empty)
            {
                result.reason = "missing-assertion-identity".into();
                return result;
            }
            if !site.covered_by.contains(&test.id) {
                result.reason = "target-not-reached-in-owning-test".into();
                return result;
            }
            if let Some(recipe) = &hint.check {
                result.reason = "omission-check-unavailable".into();
                let selected = assertions.get(&(
                    test.id.as_str(),
                    hint.assertion_source.as_deref().unwrap(),
                    hint.assertion_method.as_deref().unwrap(),
                ));
                if recipe == "value" {
                    if let (Some(direct), Some(payload)) =
                        (&hint.direct_return_sensitivity, &hint.payload_sensitivity)
                        && (direct.status == "source-checked" || payload.status == "source-checked")
                    {
                        result.reason = "conflicting-value-models".into();
                        return result;
                    }
                    if let Some(e) = &hint.direct_return_sensitivity
                        && (hint.payload_sensitivity.is_none() || e.status == "source-checked")
                    {
                        if let Some(issue) = direct_return_evidence_issue(hint, e, site, test) {
                            result.reason = issue;
                            return result;
                        }
                        result.validation = HintValidation::AnalyzerSupported;
                        result.reason = "modeled-direct-return-sensitivity".into();
                        return result; // Exact predicate outcomes only, no site/MC-DC credit.
                    }
                    result.reason = "payload-sensitivity-unavailable".into();
                    let Some(e) = &hint.payload_sensitivity else {
                        return result;
                    };
                    let in_file = |s: &Option<String>| {
                        s.as_ref()
                            .is_some_and(|s| s.starts_with(&format!("{}:", site.file)))
                    };
                    if e.model != "node-closed-payload-sensitivity-v2"
                        || e.status != "source-checked"
                        || e.reason.is_some()
                        || e.scope.as_deref() != Some("closed-synchronous-test-module")
                        || e.assertion_source != hint.assertion_source
                        || !in_file(&e.target_source)
                        || !in_file(&e.change_source)
                        || e.change_text.as_ref().is_none_or(String::is_empty)
                        || e.allocations.as_ref().is_none_or(|a| {
                            a.is_empty()
                                || a.iter().any(|s| !s.starts_with(&format!("{}:", site.file)))
                        })
                        || !(site.kind == "decision" || site.category == "return")
                    {
                        result.reason = e
                            .reason
                            .clone()
                            .unwrap_or_else(|| "unsupported-payload-sensitivity-evidence".into());
                        return result;
                    }
                    let (Some(original), Some(variants)) = (&e.original, &e.variants) else {
                        return result;
                    };
                    let valid_check = |c: &PayloadCheck, original_witness: bool| {
                        let mut budget = 4096;
                        let p = &c.projection;
                        matches!(
                            c.predicate.as_str(),
                            "node-same-value" | "node-deep-strict-equality" | "node-literal-regexp"
                        ) && p.instance.starts_with(&format!("{}:", test.file))
                            && p.read_at.starts_with(&format!("{}:", test.file))
                            && p.call_source.starts_with(&format!("{}:", site.file))
                            && p.history_selections.as_ref().is_none_or(|ss| {
                                ss.iter().all(|s| {
                                    !s.source.is_empty() && s.from <= s.to && s.to <= s.input_count
                                })
                            })
                            && valid_payload(&c.actual, 0, &mut budget)
                            && valid_payload(&c.expected, 0, &mut budget)
                            && valid_payload_coercion(c)
                            && (c.predicate == "node-literal-regexp")
                                == (c.expected["kind"] == "substring-pattern")
                            && match payload_predicate(c) {
                                Some(eq) => {
                                    c.outcome == if eq { "not-rejected" } else { "rejected" }
                                }
                                None => {
                                    original_witness
                                        && c.outcome == "witnessed-pass"
                                        && matches!(
                                            c.actual["kind"].as_str(),
                                            Some("opaque-string" | "quoted-string")
                                        )
                                        && matches!(
                                            c.expected["kind"].as_str(),
                                            Some("string" | "substring-pattern")
                                        )
                                }
                            }
                    };
                    if !valid_check(original, true)
                        || !matches!(original.outcome.as_str(), "not-rejected" | "witnessed-pass")
                        || !matches!(
                            (
                                hint.assertion_method.as_deref(),
                                original.predicate.as_str()
                            ),
                            (Some("equal" | "strictEqual"), "node-same-value")
                                | (
                                    Some("deepEqual" | "deepStrictEqual"),
                                    "node-deep-strict-equality"
                                )
                                | (Some("match"), "node-literal-regexp")
                        )
                    {
                        result.reason = "payload-assertion-not-modelled".into();
                        return result;
                    }
                    // Unlike the legacy boundary heuristic, the source interpreter
                    // derives the exact argument through history aliases. The owning
                    // passing witness above remains mandatory; no observation is invented.
                    let names: Vec<_> = variants.iter().map(|v| v.change.as_str()).collect();
                    let mut sorted = names;
                    sorted.sort();
                    if !(sorted == ["map-callback-empty"]
                        || sorted == ["condition-false", "condition-inverted", "condition-true"])
                        || variants.iter().any(|v| match v.status.as_str() {
                            "unresolved" => {
                                v.reason.as_ref().is_none_or(String::is_empty) || v.check.is_some()
                            }
                            "source-checked" => {
                                v.reason.is_some()
                                    || v.check.as_ref().is_none_or(|c| {
                                        !valid_check(c, false)
                                            || c.expected != original.expected
                                            || c.predicate != original.predicate
                                            || c.projection.instance != original.projection.instance
                                            || c.projection.argument_index
                                                != original.projection.argument_index
                                            || c.projection.read_at != original.projection.read_at
                                    })
                            }
                            _ => true,
                        })
                    {
                        result.reason = "inconsistent-payload-sensitivity-variants".into();
                        return result;
                    }
                    if variants.iter().all(|v| v.status != "source-checked") {
                        return result;
                    }
                    result.validation = HintValidation::AnalyzerSupported;
                    result.reason = "modeled-payload-sensitivity".into();
                    return result; // Explicit variant answers only; no general site credit.
                }
                if selected.is_none_or(|observations| {
                    !observations.iter().any(|ob| {
                        ob.mock.as_ref().is_some_and(|m| m.kind == "call-count")
                            && ob
                                .comparison
                                .as_ref()
                                .is_some_and(|c| c.predicate == "node-same-value")
                    })
                }) {
                    result.reason = "omission-count-assertion-not-modelled".into();
                    return result;
                }
                if recipe == "count" {
                    result.reason = "count-sensitivity-unavailable".into();
                    let Some(check) = &hint.count_sensitivity else {
                        return result;
                    };
                    let in_file = |s: &Option<String>| {
                        s.as_ref()
                            .is_some_and(|s| s.starts_with(&format!("{}:", site.file)))
                    };
                    if check.model != "node-closed-count-sensitivity-v1"
                        || check.status != "source-checked"
                        || check.reason.is_some()
                        || check.scope.as_deref() != Some("closed-synchronous-test-module")
                        || check.assertion_source != hint.assertion_source
                        || !in_file(&check.target_source)
                        || !in_file(&check.condition_source)
                        || check.condition_text.as_ref().is_none_or(String::is_empty)
                        || check.instance.as_ref().is_none_or(String::is_empty)
                        || check.allocations.as_ref().is_none_or(|a| {
                            a.is_empty()
                                || a.iter().any(|s| !s.starts_with(&format!("{}:", site.file)))
                        })
                        || !(site.kind == "decision" || site.category == "return")
                    {
                        result.reason = check
                            .reason
                            .clone()
                            .unwrap_or_else(|| "unsupported-count-sensitivity-evidence".into());
                        return result;
                    }
                    let (Some(expected), Some(original), Some(variants)) =
                        (check.expected_count, check.original_count, &check.variants)
                    else {
                        return result;
                    };
                    let expected_changes =
                        ["condition-true", "condition-false", "condition-inverted"];
                    if expected != original
                        || variants.len() != 3
                        || !expected_changes
                            .iter()
                            .all(|name| variants.iter().filter(|v| v.change == *name).count() == 1)
                        || variants.iter().any(|v| match v.status.as_str() {
                            "source-checked" => {
                                v.reason.is_some()
                                    || v.count.is_none()
                                    || v.outcome.as_deref()
                                        != Some(if v.count == Some(expected) {
                                            "not-rejected"
                                        } else {
                                            "rejected"
                                        })
                            }
                            "unresolved" => {
                                v.reason.as_ref().is_none_or(String::is_empty)
                                    || v.count.is_some()
                                    || v.outcome.is_some()
                            }
                            _ => true,
                        })
                    {
                        result.reason = "inconsistent-count-sensitivity-variants".into();
                        return result;
                    }
                    if variants.iter().all(|v| v.status != "source-checked") {
                        return result;
                    }
                    result.validation = HintValidation::AnalyzerSupported;
                    result.reason = "modeled-count-sensitivity".into();
                    // Explicit per-variant answers only; no join/whole-site credit.
                    return result;
                }
                let Some(check) = &hint.call_omission else {
                    return result;
                };
                if recipe != "missing-call"
                    || check.model != "node-first-test-call-omission-v1"
                    || check.status != "source-checked"
                    || check.reason.is_some()
                    || check.scope.as_deref() != Some("first-synchronous-test")
                    || check.assertion_source != hint.assertion_source
                    || check
                        .call_source
                        .as_ref()
                        .is_none_or(|s| !s.starts_with(&format!("{}:", site.file)))
                    || check.callback_source.as_ref().is_none_or(String::is_empty)
                    || check.instance.as_ref().is_none_or(String::is_empty)
                    || site.kind != "effect"
                {
                    result.reason = check
                        .reason
                        .clone()
                        .unwrap_or_else(|| "unsupported-omission-evidence".into());
                    return result;
                }
                let (Some(expected), Some(original), Some(omitted)) = (
                    check.expected_count,
                    check.original_count,
                    check.omitted_count,
                ) else {
                    return result;
                };
                let outcome = if omitted != expected {
                    "rejected"
                } else {
                    "not-rejected"
                };
                if original != expected || check.outcome.as_deref() != Some(outcome) {
                    result.reason = "inconsistent-omission-counts".into();
                    return result;
                }
                result.validation = HintValidation::AnalyzerSupported;
                result.reason = format!("modeled-callback-omission-{outcome}");
                // Do not promote ordinary observations, site strength, or the join.
                return result;
            }
            if site.kind != "effect" {
                result.reason = "decision-hint-analysis-not-supported".into();
                return result;
            }
            let selected = TestFacts {
                id: test.id.clone(),
                file: test.file.clone(),
                observations: assertions
                    .get(&(
                        test.id.as_str(),
                        hint.assertion_source.as_deref().unwrap(),
                        hint.assertion_method.as_deref().unwrap(),
                    ))
                    .into_iter()
                    .flatten()
                    .map(|ob| (*ob).clone())
                    .collect(),
                sinks: test.sinks.clone(),
                rendered: test.rendered.clone(),
                witness_issues: vec![],
            };
            if selected.observations.is_empty() {
                result.reason = "assertion-operand-not-modelled".into();
                return result;
            }
            let resolution = engine.resolve_effect_for(site, vec![&selected]);
            if resolution.strength.is_some() && resolution.tests.contains(&test.id) {
                let bounds = engine.bounds_for_test(site, &selected);
                result.observations = selected
                    .observations
                    .iter()
                    .filter(|ob| {
                        engine.observation_hits(site, &selected, &bounds, ob)
                            && !(site.category == "log" && ob.pattern_shared)
                    })
                    .cloned()
                    .collect();
                result.validation = HintValidation::AnalyzerSupported;
                result.reason = "existing-effect-rules-support-this-assertion-link".into();
                result.strength = resolution.strength;
            }
            result
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Verdicts out
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Evident,
    Presence,
    Partial,
    Unresolved,
}

/// Why a site is not evident. A gap is closable by writing a test; a limit is something the frontend
/// could not follow, and an agent that writes a test for one either wastes the effort or learns to
/// satisfy the analyzer instead of the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasonKind {
    #[serde(rename = "gap:not-reached")]
    GapNotReached,
    #[serde(rename = "gap:not-asserted")]
    GapNotAsserted,
    #[serde(rename = "gap:outcome-not-asserted")]
    GapOutcomeNotAsserted,
    #[serde(rename = "gap:value-not-asserted")]
    GapValueNotAsserted,
    #[serde(rename = "limit:operand-shape")]
    LimitOperandShape,
    #[serde(rename = "limit:internal-state")]
    LimitInternalState,
    #[serde(rename = "limit:undecidable")]
    LimitUndecidable,
    #[serde(rename = "limit:assertion-witness")]
    LimitAssertionWitness,
    #[serde(rename = "limit:predicate-dependence")]
    LimitPredicateDependence,
    #[serde(rename = "limit:process-exit-link")]
    LimitProcessExitLink,
}

impl ReasonKind {
    pub fn is_limit(self) -> bool {
        matches!(
            self,
            ReasonKind::LimitOperandShape
                | ReasonKind::LimitInternalState
                | ReasonKind::LimitUndecidable
                | ReasonKind::LimitAssertionWitness
                | ReasonKind::LimitPredicateDependence
                | ReasonKind::LimitProcessExitLink
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reason {
    pub kind: ReasonKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Resolution {
    pub site: String,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strength: Option<Strength>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<Reason>,
    pub covered_by: usize,
    /// tests whose observations produced the evidence
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub tests: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub weak_only: bool,
    /// Basis of the forced-outcome flags, not global semantic certainty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitivity_basis: Option<SensitivityBasis>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_true_caught: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_false_caught: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absence_needed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_observed: Option<bool>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub witness_issues: Vec<TestWitnessIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SensitivityBasis {
    BoundedSourceModel,
    BranchObservationHeuristic,
    Unavailable,
}

// ---------------------------------------------------------------------------
// The join
// ---------------------------------------------------------------------------

struct Join<'a> {
    sites: BTreeMap<&'a str, &'a Site>,
    tests: BTreeMap<&'a str, &'a TestFacts>,
    facts: &'a Facts,
    resolved: BTreeMap<String, Resolution>,
}

/// Resolve every site. Effect sites first, since a decision's outcomes are judged by the strength of the
/// sites they control; then decisions; then internal effects derived through their dependents, which can
/// promote a site and so are followed by a second decision pass, exactly as the prototype does.
pub fn join(facts: &Facts) -> Vec<Resolution> {
    let mut join = Join {
        sites: facts.sites.iter().map(|s| (s.id.as_str(), s)).collect(),
        tests: facts.tests.iter().map(|t| (t.id.as_str(), t)).collect(),
        facts,
        resolved: BTreeMap::new(),
    };
    for site in &facts.sites {
        if site.kind == "effect" {
            let r = join.resolve_effect(site);
            join.resolved.insert(site.id.clone(), r);
        }
    }
    join.resolve_decisions();
    for site in &facts.sites {
        if site.kind != "effect" {
            continue;
        }
        let unresolved = join
            .resolved
            .get(&site.id)
            .is_some_and(|r| r.status == Status::Unresolved);
        if !unresolved || site.derive.is_empty() {
            continue;
        }
        if let Some(derived) = join.derive_internal(site) {
            join.resolved.insert(site.id.clone(), derived);
        }
    }
    join.resolve_decisions();
    facts
        .sites
        .iter()
        .filter_map(|s| {
            join.resolved.get(&s.id).cloned().map(|r| {
                let mut r = join.with_witness_issues(s, join.with_comparison_limits(s, r));
                if s.kind == "decision" {
                    r.sensitivity_basis = Some(
                        if r.stuck_true_caught.is_none() || r.stuck_false_caught.is_none() {
                            SensitivityBasis::Unavailable
                        } else if s.decision.as_ref().is_some_and(|d| d.primitive.is_some()) {
                            // Only successful primitive_sensitivity checks produce flags
                            // when a primitive model is present; invalid models do not fall
                            // through to the heuristic path.
                            SensitivityBasis::BoundedSourceModel
                        } else {
                            SensitivityBasis::BranchObservationHeuristic
                        },
                    );
                }
                r
            })
        })
        .collect()
}

impl<'a> Join<'a> {
    fn resolve_decisions(&mut self) {
        for site in &self.facts.sites {
            if site.kind == "decision" {
                let r = self.resolve_decision(site);
                self.resolved.insert(site.id.clone(), r);
            }
        }
    }

    /// The strongest strength among these sites, optionally restricted to sites whose evidence came from
    /// one of `within`: with per-test outcomes known, only a test that took an outcome can have observed
    /// the sites that outcome controls.
    fn strength_of_sites(
        &self,
        ids: &[String],
        within: Option<&BTreeSet<&str>>,
    ) -> Option<Strength> {
        let mut best = None;
        for id in ids {
            let Some(r) = self.resolved.get(id) else {
                continue;
            };
            let Some(strength) = r.strength else {
                continue;
            };
            if let Some(within) = within
                && !r.tests.iter().any(|t| within.contains(t.as_str()))
            {
                continue;
            }
            best = stronger(best, Some(strength));
        }
        best
    }

    /// The given boundaries plus what the test file's module mocks bind at this site.
    fn with_mocks(&self, site: &Site, test: &TestFacts, base: &[Boundary]) -> Vec<Boundary> {
        let mut bounds = base.to_vec();
        if let Some(per_site) = self.facts.mocks_by_test_file.get(&test.file)
            && let Some(extra) = per_site.get(&site.id)
        {
            bounds.extend(extra.iter().cloned());
        }
        bounds
    }

    /// Boundaries of a site as one test sees them: its own, its module mocks, and the DOM when the test
    /// rendered the component the site belongs to. The DOM applies to the site under judgement only: a
    /// site merely reached by its value is not read through the page that rendered something else.
    fn bounds_for_test(&self, site: &Site, test: &TestFacts) -> Vec<Boundary> {
        let mut bounds = self.with_mocks(site, test, &site.bounds);
        if (site.category == "return" || site.category == "callback-return")
            && test.rendered.contains(&site.owner)
        {
            bounds.push(Boundary {
                boundary: "dom".into(),
                facet: None,
                via: Some("rendered component".into()),
            });
        }
        bounds
    }

    /// Does this observation read a sink the test wired into production at this site?
    fn sink_hit(
        &self,
        test: &TestFacts,
        site: &Site,
        bounds: &[Boundary],
        ob: &Observation,
    ) -> bool {
        if !ob.can_constrain_value() {
            return false;
        }
        for sink in &test.sinks {
            let injected = bounds.iter().any(|b| {
                b.boundary == format!("callback:{}", sink.param)
                    && match (&sink.member, &b.facet) {
                        (None, _) => true,
                        (Some(_), None) => false,
                        (Some(member), Some(facet)) => {
                            facet == member
                                || facet.starts_with(&format!("{member}."))
                                || facet.starts_with('*')
                        }
                    }
            });
            let log_sink = site.category == "log"
                && sink.param == "logger"
                && sink
                    .member
                    .as_deref()
                    .is_none_or(|m| Some(m) == site.method.as_deref());
            // `expect(table.upsert)`: a leaf of the sink object pins only calls of that method
            let leaf = ob
                .boundary
                .strip_prefix(&format!("{}.", sink.sink))
                .and_then(|rest| rest.split('.').next_back());
            let same_sink = ob.boundary == sink.sink
                || leaf
                    .is_some_and(|leaf| site.method.as_deref().is_none_or(|method| leaf == method));
            // a sink on a dense channel still only pins the messages its constraint admits
            let message_fits = site.category != "log"
                || ob
                    .log_sites
                    .as_ref()
                    .is_none_or(|admitted| admitted.contains(&site.id));
            if (injected || log_sink) && same_sink && message_fits {
                return true;
            }
        }
        false
    }

    fn resolve_effect(&self, site: &Site) -> Resolution {
        let covering: Vec<&TestFacts> = site
            .covered_by
            .iter()
            .filter_map(|id| self.tests.get(id.as_str()).copied())
            .collect();
        self.resolve_effect_for(site, covering)
    }

    fn resolve_effect_for(&self, site: &Site, covering: Vec<&TestFacts>) -> Resolution {
        // An effect with no boundary, reaching no other site and with no per-test extra, is internal:
        // only a supported derivation can resolve it; comments are not evidence.
        let any_extra = covering
            .iter()
            .any(|t| self.bounds_for_test(site, t).len() > site.bounds.len());
        if site.bounds.iter().all(Boundary::internal) && site.reached.is_empty() && !any_extra {
            return Resolution {
                site: site.id.clone(),
                status: Status::Unresolved,
                strength: None,
                reason: Some(Reason {
                    kind: ReasonKind::LimitInternalState,
                    detail: None,
                }),
                covered_by: site.covered_by.len(),
                tests: BTreeSet::new(),
                weak_only: false,
                sensitivity_basis: None,
                stuck_true_caught: None,
                stuck_false_caught: None,
                absence_needed: None,
                value_observed: None,
                witness_issues: vec![],
            };
        }
        let mut best: Option<Strength> = None;
        let mut tests = BTreeSet::new();
        let mut strong_hit = false;
        for test in &covering {
            let bounds = self.bounds_for_test(site, test);
            for ob in &test.observations {
                if !self.observation_hits(site, test, &bounds, ob) {
                    continue;
                }
                // a regex several log sites could satisfy pins none of them individually
                if site.category == "log" && ob.pattern_shared {
                    continue;
                }
                best = stronger(best, Some(ob.strength));
                tests.insert(test.id.clone());
                if !ob.weak {
                    strong_hit = true;
                }
            }
        }
        let Some(mut best) = best else {
            let boundaries: BTreeSet<&str> =
                site.bounds.iter().map(|b| b.boundary.as_str()).collect();
            let reason = if site.covered_by.is_empty() {
                Reason {
                    kind: ReasonKind::GapNotReached,
                    detail: None,
                }
            } else if !site.unmodelled_shapes.is_empty() {
                Reason {
                    kind: ReasonKind::LimitOperandShape,
                    detail: Some(site.unmodelled_shapes.join("; ")),
                }
            } else {
                Reason {
                    kind: ReasonKind::GapNotAsserted,
                    detail: Some(boundaries.into_iter().collect::<Vec<_>>().join("|")),
                }
            };
            return Resolution {
                site: site.id.clone(),
                status: Status::Unresolved,
                strength: None,
                reason: Some(reason),
                covered_by: site.covered_by.len(),
                tests: BTreeSet::new(),
                weak_only: false,
                sensitivity_basis: None,
                stuck_true_caught: None,
                stuck_false_caught: None,
                absence_needed: None,
                value_observed: None,
                witness_issues: vec![],
            };
        };
        // Returning one of several pre-built objects: downstream observations show that *a* value
        // arrived, not which one.
        if site.object_valued_return {
            best = Strength::Presence;
        }
        Resolution {
            site: site.id.clone(),
            status: if best == Strength::Presence {
                Status::Presence
            } else {
                Status::Evident
            },
            strength: Some(best),
            reason: (best == Strength::Presence).then_some(Reason {
                kind: ReasonKind::GapValueNotAsserted,
                detail: None,
            }),
            covered_by: site.covered_by.len(),
            tests,
            weak_only: !strong_hit,
            sensitivity_basis: None,
            stuck_true_caught: None,
            stuck_false_caught: None,
            absence_needed: None,
            value_observed: None,
            witness_issues: vec![],
        }
    }

    /// The same boundary/instance rules used by positive observations. Reusing
    /// them for limits must never turn a suppressed observation into credit.
    fn observation_hits(
        &self,
        site: &Site,
        test: &TestFacts,
        bounds: &[Boundary],
        ob: &Observation,
    ) -> bool {
        if bounds.iter().any(|b| ob.matches(site, b)) || self.sink_hit(test, site, &site.bounds, ob)
        {
            return true;
        }
        site.reached.iter().any(|id| {
            let Some(reached) = self.sites.get(id.as_str()) else {
                return false;
            };
            if !reached.covered_by.contains(&test.id) {
                return false;
            }
            let bounds = self.with_mocks(reached, test, &reached.direct_bounds);
            bounds
                .iter()
                .any(|b| !b.internal() && ob.matches(reached, b))
                || self.sink_hit(test, reached, &bounds, ob)
        })
    }

    /// Over-approximate which rejected observations could affect a candidate,
    /// walking only its recorded flow/control/derivation edges. This explains
    /// uncertainty; it is not a new proof of dependence. Root test ownership is
    /// checked by the caller. No coverage requirement at a negative target:
    /// an absence assertion can observe a branch whose effect never executed.
    fn issue_reaches(
        &self,
        site: &Site,
        test: &TestFacts,
        ob: &Observation,
        seen: &mut BTreeSet<String>,
    ) -> bool {
        // A dependent comparison may have a missing witness as well as an
        // unresolved value relationship. Retain structural relevance for limit
        // reporting only; the positive join always keeps its comparison guard.
        let mut structural;
        let ob = if ob
            .comparison
            .as_ref()
            .is_some_and(|c| c.relation == "shared-input-through-await")
        {
            structural = ob.clone();
            structural.comparison = None;
            &structural
        } else {
            ob
        };
        if !seen.insert(site.id.clone()) {
            return false;
        }
        let bounds = self.bounds_for_test(site, test);
        if self.observation_hits(site, test, &bounds, ob)
            || (ob.boundary == "exit" && bounds.iter().any(|b| b.boundary == "exit"))
        {
            return true;
        }
        let mut edges = site.reached.clone();
        for dep in &site.derive {
            edges.push(dep.site.clone());
            edges.extend(dep.requires_total.iter().flatten().cloned());
        }
        if let Some(d) = &site.decision {
            edges.extend(d.carrier.iter().cloned());
            for ids in [
                &d.value_flow,
                &d.then,
                &d.early_exit_downstream,
                &d.loop_body,
            ] {
                edges.extend(ids.iter().flatten().cloned());
            }
            edges.extend(d.else_.iter().flatten().flatten().cloned());
            for entry in d.default_kept.iter().flatten() {
                edges.push(entry.write.clone());
                edges.extend(entry.dependents.iter().map(|d| d.site.clone()));
            }
            if let Some(o) = &d.object_valued {
                edges.extend(o.only_a.iter().chain(&o.only_b).cloned());
            }
        }
        edges.iter().any(|id| {
            self.sites
                .get(id.as_str())
                .is_some_and(|s| self.issue_reaches(s, test, ob, seen))
        })
    }

    /// Keep a passing dependent predicate visible as an analysis limit, not a
    /// missing assertion. Structural relevance is used only to explain limits;
    /// it never supplies a strength, caught flag or evidence test set.
    fn with_comparison_limits(&self, site: &Site, mut result: Resolution) -> Resolution {
        let untaken = site
            .decision
            .as_ref()
            .and_then(|d| d.outcomes.as_ref())
            .is_some_and(|o| {
                (result.stuck_false_caught == Some(false) && o.true_.is_empty())
                    || (result.stuck_true_caught == Some(false) && o.false_.is_empty())
            });
        if result.status == Status::Evident
            || untaken
            || !result.reason.as_ref().is_some_and(|r| {
                matches!(
                    r.kind,
                    ReasonKind::GapNotAsserted
                        | ReasonKind::GapOutcomeNotAsserted
                        | ReasonKind::GapValueNotAsserted
                )
            })
        {
            return result;
        }
        let exit_relevant = site
            .covered_by
            .iter()
            .filter_map(|id| self.tests.get(id.as_str()))
            .any(|test| {
                test.observations.iter().any(|ob| {
                    ob.boundary == "exit"
                        && self.issue_reaches(site, test, ob, &mut BTreeSet::new())
                })
            });
        if exit_relevant {
            result.reason = Some(Reason {
                kind: ReasonKind::LimitProcessExitLink,
                detail: Some("A passing assertion has exit-related source evidence, but the selected child, resolved result history and production-site link are not jointly established. See processExit in test observations.".into()),
            });
            return result;
        }
        let relevant = site
            .covered_by
            .iter()
            .filter_map(|id| self.tests.get(id.as_str()))
            .any(|test| {
                test.observations.iter().any(|ob| {
                    if ob
                        .comparison
                        .as_ref()
                        .is_none_or(|c| c.relation != "shared-input-through-await")
                    {
                        return false;
                    }
                    self.issue_reaches(site, test, ob, &mut BTreeSet::new())
                })
            });
        if relevant {
            result.reason = Some(Reason {
                kind: ReasonKind::LimitPredicateDependence,
                detail: Some("A passing assertion compares values from the same immutable input through await; its constraints on the producer value are not established. See comparison operand inputs in test observations.".into()),
            });
        }
        result
    }

    /// Final reporting pass only: strengths, statuses, caught flags, evidence
    /// test sets and the denominator are unchanged. Unknown evidence may replace
    /// an assertion gap with a limit, but never a known execution gap.
    fn with_witness_issues(&self, site: &Site, mut result: Resolution) -> Resolution {
        for id in &site.covered_by {
            let Some(test) = self.tests.get(id.as_str()) else {
                continue;
            };
            for issue in &test.witness_issues {
                let relevant = match &issue.observation {
                    None => issue.kind.applies_to_whole_test(),
                    Some(ob) => self.issue_reaches(site, test, ob, &mut BTreeSet::new()),
                };
                if relevant {
                    result.witness_issues.push(TestWitnessIssue {
                        test: id.clone(),
                        issue: issue.clone(),
                    });
                }
            }
        }
        let untaken = site
            .decision
            .as_ref()
            .and_then(|d| d.outcomes.as_ref())
            .is_some_and(|o| {
                (result.stuck_false_caught == Some(false) && o.true_.is_empty())
                    || (result.stuck_true_caught == Some(false) && o.false_.is_empty())
            });
        if result.status != Status::Evident
            && !untaken
            && result.reason.as_ref().is_some_and(|reason| {
                matches!(
                    reason.kind,
                    ReasonKind::GapNotAsserted
                        | ReasonKind::GapOutcomeNotAsserted
                        | ReasonKind::GapValueNotAsserted
                )
            })
            && result
                .witness_issues
                .iter()
                .any(|w| w.issue.kind.is_uncertain())
        {
            result.reason = Some(Reason {kind: ReasonKind::LimitAssertionWitness,
                detail: Some("Assertion evidence is unavailable or inconclusive; see witnessIssues. This is not proof that the test lacks an assertion.".into())});
        }
        result
    }

    /// A witness for an outcome: a test that took it and pinned that an effect did not happen, either
    /// with a negative assertion on its sink or by comparing the sink's whole call list.
    fn pinned(&self, took: &BTreeSet<&str>, targets: &[String]) -> bool {
        for id in took {
            let Some(test) = self.tests.get(id).copied() else {
                continue;
            };
            for ob in &test.observations {
                if !ob.negative && !ob.call_list {
                    continue;
                }
                for target_id in targets {
                    let Some(target) = self.sites.get(target_id.as_str()) else {
                        continue;
                    };
                    let bounds = self.with_mocks(target, test, &target.bounds);
                    if bounds
                        .iter()
                        .any(|b| !b.internal() && ob.matches(target, b))
                        || self.sink_hit(test, target, &bounds, ob)
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn primitive_sensitivity(
        &self,
        site: &Site,
        d: &DecisionFacts,
        p: &PrimitiveDecision,
    ) -> Option<(bool, bool)> {
        if p.model != "js-primitive-decision-v1" || p.source.is_empty() || p.checks.is_empty() {
            return None;
        }
        let outcomes = d.outcomes.as_ref()?;
        let covered: BTreeSet<_> = site.covered_by.iter().collect();
        let checked: BTreeSet<_> = p.checks.iter().map(|c| &c.test).collect();
        if covered != checked || checked.len() != p.checks.len() {
            return None;
        }
        let mut stuck_true = false;
        let mut stuck_false = false;
        for c in &p.checks {
            if outcomes.true_.contains(&c.test) != c.original_outcome
                || outcomes.false_.contains(&c.test) == c.original_outcome
            {
                return None;
            }
            let test = self.tests.get(c.test.as_str())?;
            if !test.witness_issues.is_empty() || test.observations.len() != 1 {
                return None;
            }
            let ob = &test.observations[0];
            let comparison = ob.comparison.as_ref()?;
            if !ob.can_constrain_value()
                || ob.weak
                || ob.boundary != format!("return:{}", site.owner)
                || ob.assertion_source.as_ref() != Some(&c.assertion_source)
                || comparison.predicate != c.predicate
                || comparison.expected.value.as_ref() != Some(&c.expected)
            {
                return None;
            }
            let accepts = |actual: &SourcePrimitive| -> Option<bool> {
                let same = actual.same_value(&c.expected)?;
                match c.predicate.as_str() {
                    "node-same-value" => Some(same),
                    "node-not-same-value" => Some(!same),
                    _ => None,
                }
            };
            if !accepts(if c.original_outcome {
                &p.when_true
            } else {
                &p.when_false
            })? {
                return None;
            }
            stuck_true |= !accepts(&p.when_true)?;
            stuck_false |= !accepts(&p.when_false)?;
        }
        Some((stuck_true, stuck_false))
    }

    fn resolve_decision(&self, site: &Site) -> Resolution {
        let covering = site.covered_by.len();
        let empty = DecisionFacts::default();
        let d = site.decision.as_ref().unwrap_or(&empty);
        let unresolved = |reason: Reason, stuck: bool| Resolution {
            site: site.id.clone(),
            status: Status::Unresolved,
            strength: None,
            reason: Some(reason),
            covered_by: covering,
            tests: BTreeSet::new(),
            weak_only: false,
            sensitivity_basis: None,
            stuck_true_caught: stuck.then_some(false),
            stuck_false_caught: stuck.then_some(false),
            absence_needed: stuck.then_some(false),
            value_observed: None,
            witness_issues: vec![],
        };
        if let Some(primitive) = &d.primitive {
            let Some((stuck_true, stuck_false)) = self.primitive_sensitivity(site, d, primitive)
            else {
                return unresolved(
                    Reason {
                        kind: ReasonKind::LimitOperandShape,
                        detail: Some("invalid or incomplete primitive decision evidence".into()),
                    },
                    false,
                );
            };
            let status = match (stuck_true, stuck_false) {
                (true, true) => Status::Evident,
                (false, false) => Status::Unresolved,
                _ => Status::Partial,
            };
            return Resolution {
                site: site.id.clone(), status,
                strength: (status == Status::Evident).then_some(Strength::Value),
                reason: (status != Status::Evident).then(|| Reason {
                    kind: ReasonKind::GapOutcomeNotAsserted,
                    detail: Some(format!("source-checked primitive branches: forcing {} remains accepted by every modeled assertion",
                        match (stuck_true, stuck_false) { (false, false) => "either outcome", (false, true) => "true", _ => "false" })),
                }),
                covered_by: covering, tests: BTreeSet::new(), weak_only: false,
                sensitivity_basis: None,
                stuck_true_caught: Some(stuck_true), stuck_false_caught: Some(stuck_false),
                absence_needed: Some(false), value_observed: None, witness_issues: vec![],
            };
        }
        // A ternary between two pre-built objects is distinguishable only through the sites just one of
        // them reaches.
        if let Some(object_valued) = &d.object_valued {
            let only_a = self.strength_of_sites(&object_valued.only_a, None);
            let only_b = self.strength_of_sites(&object_valued.only_b, None);
            if only_a.is_some_and(Strength::caught) || only_b.is_some_and(Strength::caught) {
                return Resolution {
                    site: site.id.clone(),
                    status: Status::Evident,
                    strength: Some(Strength::Value),
                    reason: None,
                    covered_by: covering,
                    tests: BTreeSet::new(),
                    weak_only: false,
                    sensitivity_basis: None,
                    stuck_true_caught: Some(true),
                    stuck_false_caught: Some(true),
                    absence_needed: Some(false),
                    value_observed: None,
                    witness_issues: vec![],
                };
            }
            return unresolved(
                Reason {
                    kind: ReasonKind::LimitUndecidable,
                    detail: Some("object-valued branches with no branch-specific site".into()),
                },
                true,
            );
        }
        let has_shape = d.carrier.is_some()
            || d.value_flow.is_some()
            || d.then.is_some()
            || d.object_valued.is_some();
        if !has_shape {
            return unresolved(
                Reason {
                    kind: ReasonKind::LimitUndecidable,
                    detail: Some("decision context not found".into()),
                },
                false,
            );
        }
        let t_true: Option<BTreeSet<&str>> = d
            .outcomes
            .as_ref()
            .map(|o| o.true_.iter().map(String::as_str).collect());
        let t_false: Option<BTreeSet<&str>> = d
            .outcomes
            .as_ref()
            .map(|o| o.false_.iter().map(String::as_str).collect());
        let selected: Option<BTreeSet<&str>> = d
            .selected
            .as_ref()
            .map(|s| s.iter().map(String::as_str).collect());
        let mut then_s;
        let mut else_s = None;
        let mut value_observed = None;
        if let Some(carrier) = &d.carrier {
            let ids = [carrier.clone()];
            then_s = self.strength_of_sites(&ids, selected.as_ref().or(t_true.as_ref()));
            else_s = self.strength_of_sites(&ids, selected.as_ref().or(t_false.as_ref()));
            if selected.is_some() {
                value_observed = Some(
                    self.strength_of_sites(&ids, None)
                        .is_some_and(Strength::caught),
                );
            }
        } else if let Some(flow) = &d.value_flow {
            then_s = self.strength_of_sites(flow, selected.as_ref().or(t_true.as_ref()));
            else_s = self.strength_of_sites(flow, selected.as_ref().or(t_false.as_ref()));
            if selected.is_some() {
                value_observed = Some(
                    self.strength_of_sites(flow, None)
                        .is_some_and(Strength::caught),
                );
            }
        } else {
            let then_ids = d.then.clone().unwrap_or_default();
            then_s = self.strength_of_sites(&then_ids, t_true.as_ref());
            let else_ids = d.else_.clone().flatten();
            if let Some(else_ids) = &else_ids {
                else_s = self.strength_of_sites(else_ids, t_false.as_ref());
            }
            // An early exit is witnessed by a test that took it and asserted that a downstream effect of
            // the same function did not happen, or that read the early return by value.
            if !then_s.is_some_and(Strength::caught)
                && let (Some(took), Some(downstream)) = (&t_true, &d.early_exit_downstream)
            {
                let mut witnessed = self.pinned(took, downstream);
                if !witnessed {
                    witnessed = took.iter().any(|id| {
                        self.tests.get(id).is_some_and(|test| {
                            test.observations.iter().any(|ob| {
                                ob.can_constrain_value()
                                    && !ob.negative
                                    && ob.boundary == format!("return:{}", site.owner)
                                    && ob.strength.caught()
                            })
                        })
                    });
                }
                if witnessed {
                    then_s = Some(Strength::Value);
                }
            }
            // Loop control decides which iterations run, which is visible only in the calls the body
            // makes: a pinned call list would have changed had the decision gone the other way.
            if let Some(body) = &d.loop_body {
                if !then_s.is_some_and(Strength::caught)
                    && let Some(took) = &t_true
                    && self.pinned(took, body)
                {
                    then_s = Some(Strength::Value);
                }
                if !else_s.is_some_and(Strength::caught)
                    && let Some(took) = &t_false
                    && self.pinned(took, body)
                {
                    else_s = Some(Strength::Value);
                }
            }
            // `if (opt) this.x = opt` with no else keeps the field's default when the condition is false:
            // a test in which it was false, observing a dependent of the field by value, would have seen
            // the write's value there had the condition been stuck true.
            if else_ids.is_none()
                && !else_s.is_some_and(Strength::caught)
                && let (Some(t_false), Some(entries)) = (&t_false, &d.default_kept)
                && !t_false.is_empty()
            {
                for entry in entries {
                    let kept = entry.dependents.iter().any(|dep| {
                        self.resolved.get(&dep.site).is_some_and(|r| {
                            r.strength.is_some_and(Strength::caught)
                                && r.tests.iter().any(|t| t_false.contains(t.as_str()))
                        })
                    });
                    if kept {
                        else_s = Some(Strength::Value);
                        break;
                    }
                }
            }
        }
        let absence_needed = d.carrier.is_none() && d.then.is_some() && d.else_ == Some(None);
        let stuck_false_caught = then_s.is_some_and(Strength::caught);
        // With no else branch, "this must not happen" is still pinned when the branch's effects are
        // asserted with total equality: a spurious occurrence shows up. With outcome data that needs at
        // least one test in which the condition really was false.
        let absence_covered = absence_needed
            && then_s == Some(Strength::Total)
            && t_false.as_ref().is_none_or(|t| !t.is_empty());
        let stuck_true_caught = else_s.is_some_and(Strength::caught) || absence_covered;
        let status = match (stuck_false_caught, stuck_true_caught) {
            (true, true) => Status::Evident,
            (false, false) => Status::Unresolved,
            _ => Status::Partial,
        };
        let weakest = match (then_s, else_s) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let reason = (status != Status::Evident).then(|| {
            let mut missing = Vec::new();
            if !stuck_false_caught {
                missing.push("true");
            }
            if !stuck_true_caught {
                missing.push("false");
            }
            let untaken: Vec<&str> = missing
                .iter()
                .copied()
                .filter(|side| {
                    let set = if *side == "true" { &t_true } else { &t_false };
                    set.as_ref().is_some_and(|s| s.is_empty())
                })
                .collect();
            if covering == 0 {
                Reason {
                    kind: ReasonKind::GapNotReached,
                    detail: None,
                }
            } else if !untaken.is_empty() {
                Reason {
                    kind: ReasonKind::GapOutcomeNotAsserted,
                    detail: Some(format!(
                        "no test takes the {} outcome",
                        untaken.join(" or ")
                    )),
                }
            } else if !site.unmodelled_shapes.is_empty() {
                Reason {
                    kind: ReasonKind::LimitOperandShape,
                    detail: Some(site.unmodelled_shapes.join("; ")),
                }
            } else {
                Reason {
                    kind: ReasonKind::GapOutcomeNotAsserted,
                    detail: Some(format!(
                        "the {} outcome is taken but nothing asserts its effects",
                        missing.join(" and ")
                    )),
                }
            }
        });
        Resolution {
            site: site.id.clone(),
            status,
            strength: (status == Status::Evident).then_some(weakest).flatten(),
            reason,
            covered_by: covering,
            tests: BTreeSet::new(),
            weak_only: false,
            sensitivity_basis: None,
            stuck_true_caught: Some(stuck_true_caught),
            stuck_false_caught: Some(stuck_false_caught),
            absence_needed: Some(absence_needed),
            value_observed,
            witness_issues: vec![],
        }
    }

    /// An internal effect is observed through the sites that depend on it, capped at value strength: the
    /// dependent proves the state changed, not what it changed to. Needs a test covering both ends.
    fn derive_internal(&self, site: &Site) -> Option<Resolution> {
        let mut best = None;
        let mut tests = BTreeSet::new();
        for dep in &site.derive {
            if dep.site == site.id {
                continue;
            }
            if let Some(callbacks) = &dep.requires_total
                && !callbacks.iter().any(|id| {
                    self.resolved
                        .get(id)
                        .is_some_and(|r| r.strength == Some(Strength::Total))
                })
            {
                continue;
            }
            let Some(dependent) = self.sites.get(dep.site.as_str()) else {
                continue;
            };
            let strength = match dep.strength {
                Some(strength) => Some(strength),
                None => {
                    let r = self.resolved.get(&dep.site);
                    match r {
                        // A decision the tests pin in either direction proves the state reached it, so
                        // it witnesses the write at value strength whatever its own strength says.
                        Some(r) if dependent.kind == "decision" => (r.status == Status::Evident
                            || r.status == Status::Partial)
                            .then_some(Strength::Value),
                        Some(r) => r.strength,
                        None => None,
                    }
                }
            };
            let Some(strength) = strength else {
                continue;
            };
            let co_covered = site
                .covered_by
                .iter()
                .any(|t| dependent.covered_by.contains(t));
            if !co_covered {
                continue;
            }
            best = stronger(best, Some(strength.min(Strength::Value)));
            if let Some(r) = self.resolved.get(&dep.site) {
                for t in &r.tests {
                    if site.covered_by.contains(t) {
                        tests.insert(t.clone());
                    }
                }
            }
        }
        let best = best?;
        Some(Resolution {
            site: site.id.clone(),
            status: if best == Strength::Presence {
                Status::Presence
            } else {
                Status::Evident
            },
            strength: Some(best),
            reason: (best == Strength::Presence).then_some(Reason {
                kind: ReasonKind::GapValueNotAsserted,
                detail: None,
            }),
            covered_by: site.covered_by.len(),
            tests,
            weak_only: false,
            sensitivity_basis: None,
            stuck_true_caught: None,
            stuck_false_caught: None,
            absence_needed: None,
            value_observed: None,
            witness_issues: vec![],
        })
    }
}

/// The metric: of the contractual sites, how many are evident.
pub fn summary(sites: &[Site], resolutions: &[Resolution]) -> Summary {
    let by_id: BTreeMap<&str, &Resolution> =
        resolutions.iter().map(|r| (r.site.as_str(), r)).collect();
    let mut summary = Summary::default();
    for site in sites {
        if site.classification != "contractual" {
            continue;
        }
        let Some(r) = by_id.get(site.id.as_str()) else {
            continue;
        };
        summary.contractual += 1;
        match r.status {
            Status::Evident => summary.evident += 1,
            Status::Partial => summary.partial += 1,
            Status::Presence => summary.presence += 1,
            Status::Unresolved => summary.unresolved += 1,
        }
        if let Some(reason) = &r.reason {
            if reason.kind.is_limit() {
                summary.limits += 1;
            } else if r.status != Status::Evident {
                summary.gaps += 1;
            }
        }
    }
    summary
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub contractual: usize,
    pub evident: usize,
    pub partial: usize,
    pub presence: usize,
    pub unresolved: usize,
    pub gaps: usize,
    pub limits: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(id: &str, category: &str, bounds: Vec<Boundary>, covered: &[&str]) -> Site {
        Site {
            id: id.into(),
            file: "src/a.ts".into(),
            line: 1,
            kind: "effect".into(),
            category: category.into(),
            classification: "contractual".into(),
            owner: "handler".into(),
            method: None,
            bounds: bounds.clone(),
            direct_bounds: bounds,
            reached: vec![],
            covered_by: covered.iter().map(|s| (*s).into()).collect(),
            object_valued_return: false,
            unmodelled_shapes: vec![],
            decision: None,
            derive: vec![],
        }
    }

    fn boundary(name: &str) -> Boundary {
        Boundary {
            boundary: name.into(),
            facet: None,
            via: None,
        }
    }

    fn observation(boundary: &str, strength: Strength) -> Observation {
        Observation {
            boundary: boundary.into(),
            facet: None,
            strength,
            where_: None,
            assertion_source: None,
            assertion_method: None,
            negative: false,
            call_list: false,
            mock: None,
            comparison: None,
            process_exit: None,
            weak: false,
            log_sites: None,
            pattern_shared: false,
        }
    }

    fn test(id: &str, observations: Vec<Observation>) -> TestFacts {
        TestFacts {
            id: id.into(),
            file: "tests/a.test.ts".into(),
            observations,
            sinks: vec![],
            rendered: vec![],
            witness_issues: vec![],
        }
    }

    fn facts(sites: Vec<Site>, tests: Vec<TestFacts>) -> Facts {
        Facts {
            schema: 1,
            sites,
            tests,
            mocks_by_test_file: BTreeMap::new(),
        }
    }

    fn rejected(kind: WitnessIssueKind, target: Option<&str>) -> WitnessIssue {
        WitnessIssue {
            kind,
            source: Some("tests/a.test.ts:7:3".into()),
            operation: Some("equal".into()),
            observation: target.map(|target| observation(target, Strength::Total)),
        }
    }

    fn primitive_fixture(
        a: SourcePrimitive,
        b: SourcePrimitive,
        predicate: &str,
        expected_a: SourcePrimitive,
        expected_b: SourcePrimitive,
    ) -> Facts {
        let mut tests = vec![];
        let mut checks = vec![];
        for (id, outcome, expected) in [("T1", true, expected_a), ("T2", false, expected_b)] {
            let source = format!("tests/a.test.ts:{id}:3");
            let mut ob = observation("return:handler", Strength::Value);
            ob.assertion_source = Some(source.clone());
            ob.comparison = Some(Comparison {
                predicate: predicate.into(),
                relation: "unresolved".into(),
                actual: ComparisonOperand {
                    source: "tests/a:10:20".into(),
                    binding: None,
                    input: None,
                    value: None,
                },
                expected: ComparisonOperand {
                    source: "tests/a:22:23".into(),
                    binding: None,
                    input: None,
                    value: Some(expected.clone()),
                },
            });
            tests.push(test(id, vec![ob]));
            checks.push(PrimitiveDecisionCheck {
                test: id.into(),
                assertion_source: source,
                predicate: predicate.into(),
                expected,
                original_outcome: outcome,
            });
        }
        let mut decision = site("D", "condition", vec![], &["T1", "T2"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            primitive: Some(PrimitiveDecision {
                model: "js-primitive-decision-v1".into(),
                source: "src/a:0:40".into(),
                when_true: a,
                when_false: b,
                checks,
            }),
            else_: Some(Some(vec![])),
            outcomes: Some(Outcomes {
                true_: vec!["T1".into()],
                false_: vec!["T2".into()],
            }),
            ..Default::default()
        });
        facts(vec![decision], tests)
    }

    #[test]
    fn primitive_sensitivity_preserves_types_signed_zero_and_predicate_acceptance() {
        let n = |v: &str| SourcePrimitive::Number(v.into());
        for (a, b, predicate, x, y, caught) in [
            (n("7"), n("7"), "node-same-value", n("7"), n("7"), false),
            (n("1"), n("2"), "node-same-value", n("1"), n("2"), true),
            (n("1"), n("2"), "node-not-same-value", n("0"), n("0"), false),
            (n("-0"), n("0"), "node-same-value", n("-0"), n("0"), true),
            (n("1e0"), n("1"), "node-same-value", n("1"), n("1"), false),
            (
                SourcePrimitive::String("1".into()),
                n("1"),
                "node-same-value",
                SourcePrimitive::String("1".into()),
                n("1"),
                true,
            ),
            (
                SourcePrimitive::Boolean(true),
                SourcePrimitive::Boolean(false),
                "node-same-value",
                SourcePrimitive::Boolean(true),
                SourcePrimitive::Boolean(false),
                true,
            ),
            (
                SourcePrimitive::Null,
                SourcePrimitive::Null,
                "node-same-value",
                SourcePrimitive::Null,
                SourcePrimitive::Null,
                false,
            ),
        ] {
            let f = primitive_fixture(a, b, predicate, x, y);
            let encoded = serde_json::to_value(&f).unwrap();
            let decoded: Facts = serde_json::from_value(encoded).unwrap();
            assert_eq!(f, decoded);
            let r = join(&decoded);
            assert_eq!(
                r[0].sensitivity_basis,
                Some(SensitivityBasis::BoundedSourceModel)
            );
            assert_eq!(r[0].stuck_true_caught, Some(caught));
            assert_eq!(r[0].stuck_false_caught, Some(caught));
            assert_eq!(
                r[0].status,
                if caught {
                    Status::Evident
                } else {
                    Status::Unresolved
                }
            );
        }
    }

    #[test]
    fn primitive_sensitivity_rejects_incomplete_or_inconsistent_evidence() {
        let n = |v: &str| SourcePrimitive::Number(v.into());
        let base = primitive_fixture(n("1"), n("2"), "node-same-value", n("1"), n("2"));
        for case in 0..10 {
            let mut f = base.clone();
            let d = f.sites[0].decision.as_mut().unwrap();
            let p = d.primitive.as_mut().unwrap();
            match case {
                0 => {
                    p.checks.pop();
                }
                1 => p.checks.push(p.checks[0].clone()),
                2 => p.checks[0].assertion_source = "elsewhere".into(),
                3 => p.checks[0].expected = n("42"),
                4 => p.model = "unknown".into(),
                5 => p.when_true = n("NaN"),
                6 => p.when_false = n("Infinity"),
                7 => d.outcomes.as_mut().unwrap().true_.clear(),
                8 => f.tests[0]
                    .witness_issues
                    .push(rejected(WitnessIssueKind::CaptureUnavailable, None)),
                9 => {
                    p.checks[0].predicate = "node-loose-equality".into();
                    f.tests[0].observations[0]
                        .comparison
                        .as_mut()
                        .unwrap()
                        .predicate = "node-loose-equality".into();
                }
                _ => unreachable!(),
            }
            let r = join(&f);
            assert_eq!(r[0].status, Status::Unresolved, "case {case}");
            assert_eq!(r[0].sensitivity_basis, Some(SensitivityBasis::Unavailable));
            assert_eq!(
                r[0].reason.as_ref().unwrap().kind,
                ReasonKind::LimitOperandShape,
                "case {case}"
            );
            // Rejected evidence establishes neither rejection nor survival.
            assert_eq!(r[0].stuck_true_caught, None, "case {case}");
            assert_eq!(r[0].stuck_false_caught, None, "case {case}");
            let json = serde_json::to_value(&r[0]).unwrap();
            assert!(json.get("stuckTrueCaught").is_none(), "case {case}");
            assert!(json.get("stuckFalseCaught").is_none(), "case {case}");
        }
    }

    #[test]
    fn primitive_sensitivity_keeps_one_sided_rejection_distinct_from_unknown() {
        let n = |v: &str| SourcePrimitive::Number(v.into());
        // Both original tests pass: 1 != 0 and 2 != 1. Forcing true fails the
        // second test; forcing false is accepted by both modeled assertions.
        let f = primitive_fixture(n("1"), n("2"), "node-not-same-value", n("0"), n("1"));
        let r = join(&f);
        assert_eq!(r[0].status, Status::Partial);
        assert_eq!(
            r[0].sensitivity_basis,
            Some(SensitivityBasis::BoundedSourceModel)
        );
        assert_eq!(r[0].stuck_true_caught, Some(true));
        assert_eq!(r[0].stuck_false_caught, Some(false));
        assert_eq!(
            r[0].reason.as_ref().unwrap().kind,
            ReasonKind::GapOutcomeNotAsserted
        );
    }

    #[test]
    fn sensitivity_basis_does_not_promote_legacy_decisions_or_effects() {
        let mut d = site("D", "condition", vec![], &["T"]);
        d.kind = "decision".into();
        let mut f = facts(
            vec![d, site("E", "return", vec![], &["T"])],
            vec![test("T", vec![])],
        );
        let r = join(&f);
        assert_eq!(r[0].sensitivity_basis, Some(SensitivityBasis::Unavailable));
        assert!(r[1].sensitivity_basis.is_none());
        assert!(
            serde_json::to_value(&r[1])
                .unwrap()
                .get("sensitivityBasis")
                .is_none()
        );
        f.sites[0].decision = Some(DecisionFacts {
            then: Some(vec![]),
            ..Default::default()
        });
        let r = join(&f);
        assert_eq!(
            r[0].sensitivity_basis,
            Some(SensitivityBasis::BranchObservationHeuristic)
        );
        assert_eq!(
            serde_json::to_value(&r[0]).unwrap()["sensitivityBasis"],
            "branch-observation-heuristic"
        );
    }

    fn pragma_hint() -> PragmaHint {
        PragmaHint {
            id: "hint-1".into(),
            where_: "tests/a.test.ts:6:3".into(),
            raw: "// observes: src/a.ts#handler return value".into(),
            target: Some(PragmaTarget {
                file: "src/a.ts".into(),
                function: "handler".into(),
                snippet: Some("return value".into()),
                via: None,
            }),
            candidate_sites: vec!["S1".into()],
            issue: None,
            test: Some("T1".into()),
            assertion_source: Some("tests/a.test.ts:7:3".into()),
            assertion_method: Some("equal".into()),
            witness: "passed".into(),
            witness_issue: None,
            awaited_observation: None,
            check: None,
            call_omission: None,
            count_sensitivity: None,
            payload_sensitivity: None,
            direct_return_sensitivity: None,
        }
    }

    #[test]
    fn omission_checks_require_own_count_witness_and_never_promote_the_site() {
        let mut hint = pragma_hint();
        hint.check = Some("missing-call".into());
        hint.call_omission = Some(
            serde_json::from_value(serde_json::json!({
                "model": "node-first-test-call-omission-v1", "status":"source-checked",
                "scope":"first-synchronous-test", "outcome":"rejected",
                "assertionSource":"tests/a.test.ts:7:3", "callSource":"src/a.ts:4:3",
                "callbackSource":"src/a.ts:3:3", "instance":"tests/a.test.ts:2:3",
                "expectedCount":1, "originalCount":1, "omittedCount":0
            }))
            .unwrap(),
        );
        let mut ob = observation("stdout", Strength::Total);
        ob.assertion_source = hint.assertion_source.clone();
        ob.assertion_method = hint.assertion_method.clone();
        ob.mock = Some(MockProjection {
            target: "console.log".into(),
            kind: "call-count".into(),
            path: vec!["mock".into(), "callCount()".into()],
            count_evidence: None,
        });
        ob.comparison = Some(
            serde_json::from_value(serde_json::json!({
                "predicate":"node-same-value", "relation":"distinct-or-unknown",
                "actual":{"source":"tests/a.test.ts:7:16"}, "expected":{"source":"tests/a.test.ts:7:38"}
            }))
            .unwrap(),
        );
        let f = facts(
            vec![site("S1", "log", vec![boundary("stdout")], &["T1"])],
            vec![test("T1", vec![ob])],
        );
        let joined = serde_json::to_value(join(&f)).unwrap();
        let positive = &check_pragma_hints(&f, &[hint.clone()])[0];
        assert_eq!(positive.validation, HintValidation::AnalyzerSupported);
        assert_eq!(positive.reason, "modeled-callback-omission-rejected");
        assert_eq!(positive.strength, None);
        assert_eq!(serde_json::to_value(join(&f)).unwrap(), joined);
        for case in 0..9 {
            let mut f = f.clone();
            let mut h = hint.clone();
            match case {
                0 => h.witness = "unavailable".into(),
                1 => h.assertion_source = Some("tests/a.test.ts:8:3".into()),
                2 => f.tests[0].observations.clear(),
                3 => h.call_omission.as_mut().unwrap().original_count = Some(2),
                4 => h.call_omission.as_mut().unwrap().omitted_count = Some(1),
                5 => h.call_omission.as_mut().unwrap().scope = Some("any-test".into()),
                6 => h.call_omission.as_mut().unwrap().model = "unrecognized".into(),
                7 => {
                    h.call_omission.as_mut().unwrap().call_source = Some("src/other.ts:4:3".into())
                }
                8 => f.sites[0].covered_by.clear(),
                _ => unreachable!(),
            }
            assert_eq!(
                check_pragma_hints(&f, &[h])[0].validation,
                HintValidation::Unresolved,
                "case {case}"
            );
        }
        let c = hint.call_omission.as_mut().unwrap();
        c.omitted_count = Some(1);
        c.outcome = Some("not-rejected".into());
        let negative = &check_pragma_hints(&f, &[hint])[0];
        assert_eq!(negative.validation, HintValidation::AnalyzerSupported);
        assert_eq!(negative.reason, "modeled-callback-omission-not-rejected");
        assert_eq!(negative.strength, None);
    }

    #[test]
    fn count_sensitivity_keeps_variant_scope_and_requires_own_witness() {
        let mut hint = pragma_hint();
        hint.check = Some("count".into());
        hint.count_sensitivity = Some(serde_json::from_value(serde_json::json!({
            "model":"node-closed-count-sensitivity-v1", "status":"source-checked",
            "scope":"closed-synchronous-test-module", "assertionSource":"tests/a.test.ts:7:3",
            "targetSource":"src/a.ts:4:3", "conditionSource":"src/a.ts:4:3", "conditionText":"mode === 'quiet'",
            "allocations":["src/a.ts:2:3"], "instance":"tests/a.test.ts:2:3", "expectedCount":1, "originalCount":1,
            "variants":[
                {"change":"condition-true","status":"source-checked","count":0,"outcome":"rejected"},
                {"change":"condition-false","status":"source-checked","count":1,"outcome":"not-rejected"},
                {"change":"condition-inverted","status":"unresolved","reason":"unsupported-prefix"}
            ]
        })).unwrap());
        let mut ob = observation("stdout", Strength::Total);
        ob.assertion_source = hint.assertion_source.clone();
        ob.assertion_method = hint.assertion_method.clone();
        ob.mock = Some(MockProjection {
            target: "console.log".into(),
            kind: "call-count".into(),
            path: vec!["mock".into(), "callCount()".into()],
            count_evidence: None,
        });
        ob.comparison = Some(serde_json::from_value(serde_json::json!({
            "predicate":"node-same-value", "relation":"distinct-or-unknown",
            "actual":{"source":"tests/a.test.ts:7:16"},"expected":{"source":"tests/a.test.ts:7:38"}
        })).unwrap());
        let mut s = site("S1", "condition", vec![boundary("stdout")], &["T1"]);
        s.kind = "decision".into();
        let f = facts(vec![s], vec![test("T1", vec![ob])]);
        let before = serde_json::to_value(join(&f)).unwrap();
        let result = &check_pragma_hints(&f, &[hint.clone()])[0];
        assert_eq!(result.validation, HintValidation::AnalyzerSupported);
        assert_eq!(result.reason, "modeled-count-sensitivity");
        assert_eq!(result.strength, None);
        assert_eq!(serde_json::to_value(join(&f)).unwrap(), before);
        for case in 0..12 {
            let mut h = hint.clone();
            let mut input = f.clone();
            let e = h.count_sensitivity.as_mut().unwrap();
            match case {
                0 => h.witness = "unavailable".into(),
                1 => h.assertion_source = Some("tests/a.test.ts:8:3".into()),
                2 => input.tests[0].observations.clear(),
                3 => e.original_count = Some(9),
                4 => e.variants.as_mut().unwrap()[0].count = Some(1),
                5 => e.variants.as_mut().unwrap()[2].outcome = Some("rejected".into()),
                6 => e.variants.as_mut().unwrap()[0].change = "arbitrary-edit".into(),
                7 => e.allocations = Some(vec![]),
                8 => e.target_source = Some("src/other.ts:4:3".into()),
                9 => e.scope = Some("whole-repository".into()),
                10 => input.sites[0].covered_by.clear(),
                11 => e.variants.as_mut().unwrap()[0].change = "condition-false".into(),
                _ => unreachable!(),
            }
            assert_eq!(
                check_pragma_hints(&input, &[h])[0].validation,
                HintValidation::Unresolved,
                "case {case}"
            );
        }
    }

    #[test]
    fn direct_return_variants_require_exact_scope_witness_and_consistent_values() {
        let mut hint = pragma_hint();
        hint.check = Some("value".into());
        let original = serde_json::json!({
            "predicate":"node-same-value", "actual":{"kind":"string","value":"*"},
            "expected":{"kind":"string","value":"*"}, "outcome":"not-rejected",
            "callSource":"tests/a.test.ts:7:16", "targetEvaluations":1
        });
        let mut changed = original.clone();
        changed["actual"] = serde_json::json!({"kind":"array","properties":[]});
        changed["outcome"] = "rejected".into();
        hint.direct_return_sensitivity = Some(serde_json::from_value(serde_json::json!({
            "model":"node-first-test-direct-return-v1", "status":"source-checked",
            "scope":"first-synchronous-test-prefix", "assertionSource":"tests/a.test.ts:7:3",
            "targetSource":"src/a.ts:4:3", "changeSource":"src/a.ts:4:3", "changeText":"items.length === 0",
            "original":original,
            "variants":[
                {"change":"condition-true","status":"source-checked","check":original},
                {"change":"condition-false","status":"source-checked","check":changed},
                {"change":"condition-inverted","status":"unresolved","reason":"not supported"}
            ]
        })).unwrap());
        let mut s = site("S1", "condition", vec![], &["T1"]);
        s.kind = "decision".into();
        s.line = 4;
        let mut t = test("T1", vec![]);
        t.file = "tests/a.test.ts".into();
        let f = facts(vec![s], vec![t]);
        let before = join(&f);
        let result = check_pragma_hints(&f, &[hint.clone()]).remove(0);
        assert_eq!(result.validation, HintValidation::AnalyzerSupported);
        assert!(result.strength.is_none());
        assert!(result.observations.is_empty());
        assert_eq!(join(&f), before);
        for case in 0..18 {
            let mut h = hint.clone();
            let mut ff = f.clone();
            let e = h.direct_return_sensitivity.as_mut().unwrap();
            match case {
                0 => h.witness = "unavailable".into(),
                1 => h.assertion_source = Some("tests/a.test.ts:8:3".into()),
                2 => e.original.as_mut().unwrap().outcome = "witnessed-pass".into(),
                3 => {
                    e.variants.as_mut().unwrap()[1]
                        .check
                        .as_mut()
                        .unwrap()
                        .outcome = "not-rejected".into()
                }
                4 => {
                    e.variants.as_mut().unwrap()[1]
                        .check
                        .as_mut()
                        .unwrap()
                        .expected = serde_json::json!({"kind":"undefined"})
                }
                5 => e.original.as_mut().unwrap().call_source = "tests/other.ts:7:16".into(),
                6 => e.original.as_mut().unwrap().target_evaluations = 0,
                7 => e.scope = Some("whole-suite".into()),
                8 => e.target_source = Some("src/a.ts:5:3".into()),
                9 => h.assertion_method = Some("ok".into()),
                10 => e.variants.as_mut().unwrap()[0].change = "arbitrary-edit".into(),
                11 => {
                    e.original.as_mut().unwrap().actual =
                        serde_json::json!({"kind":"opaque-string"})
                }
                12 => {
                    e.original.as_mut().unwrap().actual =
                        serde_json::json!({"kind":"string","value":"*","extra":1})
                }
                13 => e.variants.as_mut().unwrap()[2].check = e.original.clone(),
                14 => e.original.as_mut().unwrap().call_source = "tests/a.test.ts:0:0".into(),
                15 => ff.tests[0]
                    .witness_issues
                    .push(rejected(WitnessIssueKind::CaptureUnavailable, None)),
                16 => {
                    let mut issue = rejected(WitnessIssueKind::CallFailed, None);
                    issue.source = h.assertion_source.clone();
                    ff.tests[0].witness_issues.push(issue);
                }
                17 => {
                    h.payload_sensitivity = Some(
                        serde_json::from_value(
                            serde_json::json!({"model":"unused", "status":"unresolved"}),
                        )
                        .unwrap(),
                    )
                }
                _ => unreachable!(),
            }
            assert_eq!(
                check_pragma_hints(&ff, &[h])[0].validation,
                HintValidation::Unresolved,
                "case {case}"
            );
        }
        let e = hint.direct_return_sensitivity.as_mut().unwrap();
        e.change_text = Some("false".into());
        e.original = Some(
            serde_json::from_value(serde_json::json!({
                "predicate":"node-same-value", "actual":{"kind":"boolean","value":false},
                "expected":{"kind":"boolean","value":false}, "outcome":"not-rejected",
                "callSource":"tests/a.test.ts:7:16", "targetEvaluations":1
            }))
            .unwrap(),
        );
        let mut c = e.original.clone().unwrap();
        c.actual = serde_json::json!({"kind":"boolean","value":true});
        c.outcome = "rejected".into();
        e.variants = Some(vec![DirectReturnVariant {
            change: "boolean-literal-inverted".into(),
            status: "source-checked".into(),
            reason: None,
            check: Some(c),
        }]);
        assert_eq!(
            check_pragma_hints(&f, &[hint])[0].validation,
            HintValidation::AnalyzerSupported
        );
    }

    #[test]
    fn payload_sensitivity_checks_values_and_own_witness_without_legacy_links() {
        let mut hint = pragma_hint();
        hint.check = Some("value".into());
        let original = serde_json::json!({
            "predicate":"node-same-value", "actual":{"kind":"string","value":"hello"},
            "expected":{"kind":"string","value":"hello"}, "outcome":"not-rejected",
            "projection":{"instance":"tests/a.test.ts:2:3", "callSource":"src/a.ts:4:3", "callIndex":0, "argumentIndex":1, "readAt":"tests/a.test.ts:7:16"}
        });
        let mut changed = original.clone();
        changed["actual"] = serde_json::json!({"kind":"undefined"});
        changed["outcome"] = "rejected".into();
        hint.payload_sensitivity = Some(serde_json::from_value(serde_json::json!({
            "model":"node-closed-payload-sensitivity-v2", "status":"source-checked",
            "scope":"closed-synchronous-test-module", "assertionSource":"tests/a.test.ts:7:3",
            "targetSource":"src/a.ts:4:3", "changeSource":"src/a.ts:4:12", "changeText":"{ return arg; }",
            "allocations":["src/a.ts:2:3"], "original":original,
            "variants":[{"change":"map-callback-empty","status":"source-checked","check":changed}]
        })).unwrap());
        let mut s = site("S1", "return", vec![], &["T1"]);
        s.category = "return".into();
        // Conditional history aliases need not have a legacy heuristic observation.
        let mut t = test("T1", vec![]);
        t.file = "tests/a.test.ts".into();
        let f = facts(vec![s], vec![t]);
        let before = serde_json::to_value(join(&f)).unwrap();
        let result = check_pragma_hints(&f, &[hint.clone()]).remove(0);
        assert_eq!(result.validation, HintValidation::AnalyzerSupported);
        assert_eq!(result.strength, None);
        assert_eq!(serde_json::to_value(join(&f)).unwrap(), before);
        for case in 0..13 {
            let mut h = hint.clone();
            let e = h.payload_sensitivity.as_mut().unwrap();
            match case {
                0 => h.witness = "unavailable".into(),
                1 => h.assertion_source = Some("tests/a.test.ts:9:3".into()),
                2 => {
                    e.variants.as_mut().unwrap()[0]
                        .check
                        .as_mut()
                        .unwrap()
                        .outcome = "not-rejected".into()
                }
                3 => {
                    e.original.as_mut().unwrap().actual =
                        serde_json::json!({"kind":"opaque-string"})
                }
                4 => {
                    e.variants.as_mut().unwrap()[0]
                        .check
                        .as_mut()
                        .unwrap()
                        .expected = serde_json::json!({"kind":"undefined"})
                }
                5 => {
                    e.variants.as_mut().unwrap()[0]
                        .check
                        .as_mut()
                        .unwrap()
                        .projection
                        .argument_index = 9
                }
                6 => e.variants.as_mut().unwrap()[0].change = "arbitrary-edit".into(),
                7 => e.allocations = Some(vec![]),
                8 => e.original.as_mut().unwrap().projection.instance = "tests/other.ts:2:3".into(),
                9 => h.assertion_method = Some("ok".into()),
                10 => e.variants.as_mut().unwrap()[0].status = "unresolved".into(),
                11 => e.scope = Some("whole-suite".into()),
                12 => {
                    e.original.as_mut().unwrap().actual =
                        serde_json::json!({"kind":"string","value":"hello","invented":true})
                }
                _ => unreachable!(),
            }
            assert_eq!(
                check_pragma_hints(&f, &[h])[0].validation,
                HintValidation::Unresolved,
                "case {case}"
            );
        }
        let object = serde_json::json!({"kind":"object","properties":[{"name":"a","value":{"kind":"number","value":1}}]});
        let opaque = serde_json::json!({"kind":"opaque-string"});
        assert_eq!(payload_equal(&opaque, &object, true), Some(false));
        assert_eq!(payload_equal(&opaque, &opaque, true), None);
        assert_eq!(payload_equal(&object, &object, false), None);
        assert!(valid_payload(&object, 0, &mut 4096));
        assert!(!valid_payload(
            &serde_json::json!({"kind":"object","properties":[{"name":"x","value":{"kind":"null"}},{"name":"x","value":{"kind":"null"}}]}),
            0,
            &mut 4096
        ));
    }

    #[test]
    fn payload_native_predicates_keep_original_witness_separate_from_variant_proofs() {
        let mut hint = pragma_hint();
        hint.check = Some("value".into());
        hint.assertion_method = Some("match".into());
        let original = serde_json::json!({
            "predicate":"node-literal-regexp", "actual":{"kind":"opaque-string"},
            "expected":{"kind":"substring-pattern","value":"a: 1"}, "outcome":"witnessed-pass",
            "projection":{"instance":"tests/a.test.ts:2:3", "callSource":"src/a.ts:4:3", "callIndex":0, "argumentIndex":2, "readAt":"tests/a.test.ts:7:16",
                "coercion":{"source":"tests/a.test.ts:7:16", "rule":"string-identity", "input":{"kind":"opaque-string"}}}
        });
        let mut changed = original.clone();
        changed["actual"] = serde_json::json!({"kind":"string","value":"[object Object]"});
        changed["outcome"] = "rejected".into();
        changed["projection"]["coercion"] = serde_json::json!({
            "source":"tests/a.test.ts:7:16", "rule":"plain-object-default-string",
            "input":{"kind":"object","properties":[{"name":"a","value":{"kind":"number","value":1}}]}
        });
        hint.payload_sensitivity = Some(serde_json::from_value(serde_json::json!({
            "model":"node-closed-payload-sensitivity-v2", "status":"source-checked",
            "scope":"closed-synchronous-test-module", "assertionSource":"tests/a.test.ts:7:3",
            "targetSource":"src/a.ts:4:3", "changeSource":"src/a.ts:4:12", "changeText":"mode === 'verbose'",
            "allocations":["src/a.ts:2:3"], "original":original,
            "variants":[
                {"change":"condition-false","status":"source-checked","check":changed},
                {"change":"condition-true","status":"unresolved","reason":"opaque value"},
                {"change":"condition-inverted","status":"unresolved","reason":"opaque value"}
            ]
        })).unwrap());
        let mut s = site("S1", "return", vec![], &["T1"]);
        s.category = "return".into();
        let mut t = test("T1", vec![]);
        t.file = "tests/a.test.ts".into();
        let f = facts(vec![s], vec![t]);
        assert_eq!(
            check_pragma_hints(&f, &[hint.clone()])[0].validation,
            HintValidation::AnalyzerSupported
        );
        for case in 0..11 {
            let mut h = hint.clone();
            let e = h.payload_sensitivity.as_mut().unwrap();
            match case {
                0 => h.witness = "unavailable".into(),
                1 => h.assertion_source = Some("tests/a.test.ts:9:3".into()),
                2 => e.original.as_mut().unwrap().outcome = "not-rejected".into(),
                3 => e.variants.as_mut().unwrap()[0].check = e.original.clone(),
                4 => {
                    let o = e.original.as_mut().unwrap();
                    o.actual = serde_json::json!({"kind":"string","value":"does not match"});
                    o.projection.coercion = None;
                }
                5 => {
                    e.variants.as_mut().unwrap()[0]
                        .check
                        .as_mut()
                        .unwrap()
                        .projection
                        .coercion
                        .as_mut()
                        .unwrap()
                        .rule = "guess".into()
                }
                6 => {
                    e.variants.as_mut().unwrap()[0]
                        .check
                        .as_mut()
                        .unwrap()
                        .projection
                        .coercion
                        .as_mut()
                        .unwrap()
                        .source = "tests/a.test.ts:9:3".into()
                }
                7 => {
                    e.variants.as_mut().unwrap()[0]
                        .check
                        .as_mut()
                        .unwrap()
                        .projection
                        .coercion
                        .as_mut()
                        .unwrap()
                        .input = serde_json::json!({"kind":"object","properties":[{"name":"toString","value":{"kind":"string","value":"a: 1"}}]})
                }
                8 => {
                    e.original.as_mut().unwrap().expected =
                        serde_json::json!({"kind":"substring-pattern","value":"a: [0-9]"})
                }
                9 => e.model = "node-closed-payload-sensitivity-v1".into(),
                10 => h.assertion_method = Some("equal".into()),
                _ => unreachable!(),
            }
            assert_eq!(
                check_pragma_hints(&f, &[h])[0].validation,
                HintValidation::Unresolved,
                "case {case}"
            );
        }
        let quoted = serde_json::json!({"kind":"quoted-string","value":"hello"});
        assert_eq!(
            payload_equal(
                &quoted,
                &serde_json::json!({"kind":"string","value":"hello"}),
                false
            ),
            Some(false)
        );
        assert_eq!(
            payload_equal(
                &quoted,
                &serde_json::json!({"kind":"string","value":"'hello'"}),
                false
            ),
            None
        );
        assert_eq!(payload_equal(&quoted, &quoted, false), None);
        assert!(!valid_payload(
            &serde_json::json!({"kind":"quoted-string","value":"hello!"}),
            0,
            &mut 4096
        ));
    }

    #[test]
    fn source_supported_await_does_not_borrow_a_passing_assertion_phase() {
        let mut hint = pragma_hint();
        hint.awaited_observation = Some(AwaitedObservationSource {
            model: "node-child-capture-poll-v1".into(),
            factory_source: "tests/process.mjs:4:1".into(),
            predicate_source: "tests/process.mjs:21:38".into(),
            captures: vec![ObservationCaptureSource {
                stream: "stdout".into(),
                source: "tests/process.mjs:8:58".into(),
            }],
            pattern: "/ready/".into(),
        });
        // Even a supplied 'passed' hint and matching ordinary observation are
        // not a read receipt for an awaited source model.
        let mut ob = observation("return:handler", Strength::Total);
        ob.assertion_source = hint.assertion_source.clone();
        ob.assertion_method = hint.assertion_method.clone();
        let f = facts(
            vec![site("S1", "return", vec![], &["T1"])],
            vec![test("T1", vec![ob])],
        );
        let before = join(&f);
        let checks = check_pragma_hints(&f, &[hint]);
        assert_eq!(checks[0].validation, HintValidation::Unresolved);
        assert_eq!(checks[0].reason, "observation-capture-unavailable");
        assert!(checks[0].observations.is_empty());
        assert!(checks[0].strength.is_none());
        assert_eq!(join(&f), before);
    }

    #[test]
    fn pragma_hints_use_only_the_named_assertion_and_never_change_join_credit() {
        let hint = pragma_hint();
        let mut wanted = observation("return:handler", Strength::Value);
        wanted.assertion_source = hint.assertion_source.clone();
        wanted.assertion_method = hint.assertion_method.clone();
        let mut other = wanted.clone();
        other.assertion_source = Some("tests/a.test.ts:9:3".into());
        let f = facts(
            vec![site(
                "S1",
                "return",
                vec![boundary("return:handler")],
                &["T1"],
            )],
            vec![test("T1", vec![wanted.clone(), other.clone()])],
        );
        let before = join(&f);
        let checked = check_pragma_hints(&f, std::slice::from_ref(&hint));
        assert_eq!(checked[0].validation, HintValidation::AnalyzerSupported);
        assert_eq!(checked[0].origin, "user-suggested");
        assert_eq!(checked[0].strength, Some(Strength::Value));
        assert_eq!(checked[0].observations, vec![wanted.clone()]);
        assert_eq!(join(&f), before);

        let mut unrelated = f.clone();
        unrelated.tests[0].observations[0].boundary = "return:other".into();
        assert_eq!(
            join(&unrelated)[0].status,
            Status::Evident,
            "a different assertion still checks it"
        );
        assert_eq!(
            check_pragma_hints(&unrelated, std::slice::from_ref(&hint))[0].validation,
            HintValidation::Unresolved,
            "cannot borrow that assertion's credit"
        );

        let mut other_test = f.clone();
        other_test.sites[0].covered_by = vec!["T2".into()];
        other_test.tests.push(test("T2", vec![wanted]));
        assert_eq!(
            check_pragma_hints(&other_test, std::slice::from_ref(&hint))[0].reason,
            "target-not-reached-in-owning-test"
        );

        for issue in [
            "call-failed",
            "mixed-call-outcomes",
            "call-incomplete",
            "call-not-recorded",
            "capture-unavailable",
        ] {
            let mut rejected = hint.clone();
            rejected.witness_issue = Some(issue.into());
            rejected.witness = "unavailable".into();
            let check = check_pragma_hints(&f, &[rejected]).remove(0);
            assert_eq!(check.validation, HintValidation::Unresolved);
            assert_eq!(check.reason, issue);
            assert_eq!(check.strength, None);
        }
        let mut ambiguous = hint.clone();
        ambiguous.candidate_sites.push("S2".into());
        assert_eq!(
            check_pragma_hints(&f, &[ambiguous])[0].validation,
            HintValidation::Invalid
        );
        let mut no_identity = hint;
        no_identity.assertion_source = None;
        assert_eq!(
            check_pragma_hints(&f, &[no_identity])[0].reason,
            "missing-assertion-identity"
        );
    }

    #[test]
    fn pragma_hints_preserve_presence_strength_and_do_not_derive_internal_credit() {
        let hint = pragma_hint();
        let mut ob = observation("return:handler", Strength::Presence);
        ob.assertion_source = hint.assertion_source.clone();
        ob.assertion_method = hint.assertion_method.clone();
        let mut f = facts(
            vec![site(
                "S1",
                "return",
                vec![boundary("return:handler")],
                &["T1"],
            )],
            vec![test("T1", vec![ob])],
        );
        let check = check_pragma_hints(&f, std::slice::from_ref(&hint)).remove(0);
        assert_eq!(check.validation, HintValidation::AnalyzerSupported);
        assert_eq!(check.strength, Some(Strength::Presence));
        f.sites[0].bounds = vec![boundary("internal")];
        assert_eq!(
            check_pragma_hints(&f, std::slice::from_ref(&hint))[0].validation,
            HintValidation::Unresolved
        );
        f.sites[0].kind = "decision".into();
        assert_eq!(
            check_pragma_hints(&f, &[hint])[0].reason,
            "decision-hint-analysis-not-supported"
        );
    }

    #[test]
    fn unavailable_assertion_evidence_is_a_limit_without_changing_coverage_or_credit() {
        let mut f = facts(
            vec![site(
                "S",
                "return",
                vec![boundary("return:handler")],
                &["T"],
            )],
            vec![test("T", vec![])],
        );
        let before = join(&f)[0].clone();
        f.tests[0]
            .witness_issues
            .push(rejected(WitnessIssueKind::CaptureUnavailable, None));
        let after = &join(&f)[0];
        assert_eq!(
            after.reason.as_ref().unwrap().kind,
            ReasonKind::LimitAssertionWitness
        );
        assert_eq!(after.status, before.status);
        assert_eq!(after.strength, before.strength);
        assert_eq!(after.covered_by, before.covered_by);
        assert_eq!(after.tests, before.tests);
        assert_eq!(after.witness_issues[0].test, "T");
        assert_eq!(summary(&f.sites, &join(&f)).limits, 1);
        assert_eq!(summary(&f.sites, &join(&f)).gaps, 0);
    }

    #[test]
    fn unlinked_test_source_is_not_an_assertion_gap_or_pragma_permission() {
        let mut missing = test("T1", vec![]);
        missing.witness_issues.push(WitnessIssue {
            kind: WitnessIssueKind::TestSourceUnlinked,
            source: None,
            operation: None,
            observation: None,
        });
        let mut f = facts(
            vec![
                site("S1", "return", vec![boundary("return:handler")], &["T1"]),
                site("uncovered", "return", vec![boundary("return:handler")], &[]),
                site(
                    "unrelated",
                    "return",
                    vec![boundary("return:handler")],
                    &["T2"],
                ),
            ],
            vec![missing, test("T2", vec![])],
        );
        let rows = join(&f);
        assert_eq!(
            rows[0].reason.as_ref().unwrap().kind,
            ReasonKind::LimitAssertionWitness
        );
        assert_eq!(
            rows[0].witness_issues[0].issue.kind,
            WitnessIssueKind::TestSourceUnlinked
        );
        assert_eq!(rows[0].strength, None);
        assert_eq!(
            rows[1].reason.as_ref().unwrap().kind,
            ReasonKind::GapNotReached
        );
        assert_eq!(
            rows[2].reason.as_ref().unwrap().kind,
            ReasonKind::GapNotAsserted
        );
        let mut hint = pragma_hint();
        hint.assertion_source = Some("tests/a.test.ts:7:3".into());
        for recipe in [None, Some("value"), Some("count"), Some("missing-call")] {
            hint.check = recipe.map(String::from);
            let result = check_pragma_hints(&f, &[hint.clone()]).remove(0);
            assert_eq!(result.validation, HintValidation::Unresolved);
            assert_eq!(result.reason, "test-source-unlinked");
        }
        // An independent positive observation is not erased by an unlinked test.
        f.sites[0].covered_by.push("T2".into());
        f.tests[1]
            .observations
            .push(observation("return:handler", Strength::Total));
        assert_eq!(join(&f)[0].status, Status::Evident);
        assert_eq!(join(&f)[0].witness_issues.len(), 1);
    }

    #[test]
    fn self_comparisons_cannot_supply_value_absence_sink_pragma_or_early_exit_credit() {
        for (predicate, relation) in [
            "node-same-value",
            "node-loose-equality",
            "node-deep-equality",
            "node-deep-strict-equality",
        ]
        .into_iter()
        .flat_map(|predicate| {
            ["same-immutable-binding", "shared-input-through-await"]
                .map(|relation| (predicate, relation))
        }) {
            let operand = ComparisonOperand {
                source: "tests/a.test.ts:70:76".into(),
                value: None,
                binding: Some("tests/a.test.ts:20:40".into()),
                input: (relation == "shared-input-through-await").then(|| ComparisonInput {
                    binding: "tests/a.test.ts:20:40".into(),
                    awaits: vec!["tests/a.test.ts:64:76".into()],
                }),
            };
            let mut ob = observation("return:handler", Strength::Total);
            ob.comparison = Some(Comparison {
                predicate: predicate.into(),
                actual: operand.clone(),
                expected: ComparisonOperand {
                    source: "tests/a.test.ts:78:84".into(),
                    ..operand
                },
                relation: relation.into(),
            });
            ob.assertion_source = Some("tests/a.test.ts:7:3".into());
            ob.assertion_method = Some("equal".into());
            ob.call_list = true;
            let mut negative = ob.clone();
            negative.negative = true;
            let mut sink_ob = ob.clone();
            sink_ob.boundary = "sink:records".into();
            let mut negative_sink = sink_ob.clone();
            negative_sink.negative = true;
            let mut t = test("T1", vec![ob.clone(), negative, sink_ob, negative_sink]);
            t.sinks.push(SinkBinding {
                sink: "sink:records".into(),
                param: "writer".into(),
                member: None,
            });
            let mut upstream = site("S2", "return", vec![boundary("internal")], &["T1"]);
            upstream.reached = vec!["S1".into()];
            let mut decision = site("D1", "condition", vec![], &["T1", "T2"]);
            decision.kind = "decision".into();
            decision.decision = Some(DecisionFacts {
                then: Some(vec!["S4".into()]),
                else_: Some(Some(vec!["S5".into()])),
                early_exit_downstream: Some(vec!["S5".into()]),
                outcomes: Some(Outcomes {
                    true_: vec!["T1".into()],
                    false_: vec!["T2".into()],
                }),
                ..Default::default()
            });
            let mut f = facts(
                vec![
                    site("S1", "return", vec![boundary("return:handler")], &["T1"]),
                    upstream,
                    site(
                        "S3",
                        "external-call",
                        vec![boundary("callback:writer")],
                        &["T1"],
                    ),
                    site("S4", "return", vec![boundary("internal")], &["T1"]),
                    site("S5", "io-call", vec![boundary("client-message")], &["T2"]),
                    decision,
                ],
                vec![
                    t,
                    test("T2", vec![observation("client-message", Strength::Total)]),
                ],
            );
            let rows = join(&f);
            assert!(
                rows[..4]
                    .iter()
                    .all(|r| r.status == Status::Unresolved && r.strength.is_none()),
                "{predicate}"
            );
            let d = rows.iter().find(|r| r.site == "D1").unwrap();
            assert_eq!(d.status, Status::Partial);
            assert_eq!(d.stuck_false_caught, Some(false));
            assert_eq!(
                check_pragma_hints(&f, &[pragma_hint()])[0].validation,
                HintValidation::Unresolved
            );
            let joiner = Join {
                sites: f.sites.iter().map(|s| (s.id.as_str(), s)).collect(),
                tests: f.tests.iter().map(|t| (t.id.as_str(), t)).collect(),
                facts: &f,
                resolved: BTreeMap::new(),
            };
            assert!(!joiner.pinned(&BTreeSet::from(["T1"]), &["S1".into()]));
            let bytes = serde_json::to_vec(&ob).unwrap();
            assert_eq!(serde_json::from_slice::<Observation>(&bytes).unwrap(), ob);
            f.tests[0]
                .observations
                .push(observation("return:handler", Strength::Value));
            let rows = join(&f);
            assert_eq!(
                rows[0].status,
                Status::Evident,
                "independent evidence is preserved"
            );
            assert_eq!(
                rows.iter()
                    .find(|r| r.site == "D1")
                    .unwrap()
                    .stuck_false_caught,
                Some(true)
            );
        }
    }

    #[test]
    fn awaited_input_limits_preserve_execution_gaps_and_independent_evidence() {
        let mut ob = observation("return:handler", Strength::Total);
        ob.comparison = Some(Comparison {
            predicate: "node-deep-strict-equality".into(),
            actual: ComparisonOperand {
                source: "tests/a:10:20".into(),
                value: None,
                binding: None,
                input: Some(ComparisonInput {
                    binding: "tests/a:1:5".into(),
                    awaits: vec!["tests/a:10:20".into()],
                }),
            },
            expected: ComparisonOperand {
                source: "tests/a:22:25".into(),
                value: None,
                binding: Some("tests/a:1:5".into()),
                input: Some(ComparisonInput {
                    binding: "tests/a:1:5".into(),
                    awaits: vec![],
                }),
            },
            relation: "shared-input-through-await".into(),
        });
        let mut decision = site("D", "condition", vec![], &["T"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            then: Some(vec!["S".into()]),
            else_: Some(Some(vec!["S".into()])),
            outcomes: Some(Outcomes {
                true_: vec!["T".into()],
                false_: vec![],
            }),
            ..Default::default()
        });
        let mut f = facts(
            vec![
                site("S", "return", vec![boundary("return:handler")], &["T"]),
                site("untested", "return", vec![boundary("return:handler")], &[]),
                site(
                    "unrelated",
                    "return",
                    vec![boundary("return:other")],
                    &["T"],
                ),
                decision,
            ],
            vec![test("T", vec![ob.clone()])],
        );
        let rows = join(&f);
        assert_eq!(rows[0].status, Status::Unresolved);
        assert_eq!(rows[0].strength, None);
        assert!(rows[0].tests.is_empty());
        assert!(
            rows[0].witness_issues.is_empty(),
            "the witness is not rejected"
        );
        assert_eq!(
            rows[0].reason.as_ref().unwrap().kind,
            ReasonKind::LimitPredicateDependence
        );
        assert_eq!(
            rows[1].reason.as_ref().unwrap().kind,
            ReasonKind::GapNotReached
        );
        assert_eq!(
            rows[2].reason.as_ref().unwrap().kind,
            ReasonKind::GapNotAsserted
        );
        assert_eq!(
            rows[3].reason.as_ref().unwrap().kind,
            ReasonKind::GapOutcomeNotAsserted
        );
        assert_eq!(summary(&f.sites, &rows).limits, 1);
        assert_eq!(f.tests[0].observations, vec![ob.clone()]);
        f.tests[0]
            .observations
            .push(observation("return:handler", Strength::Total));
        assert_eq!(join(&f)[0].status, Status::Evident);
        f.tests[0].observations.clear();
        let mut issue = rejected(WitnessIssueKind::CallNotRecorded, Some("return:handler"));
        issue.observation = Some(ob);
        f.tests[0].witness_issues.push(issue);
        let missing = &join(&f)[0];
        assert_eq!(missing.status, Status::Unresolved);
        assert_eq!(missing.strength, None);
        assert_eq!(missing.witness_issues.len(), 1);
        assert_eq!(
            missing.reason.as_ref().unwrap().kind,
            ReasonKind::LimitAssertionWitness
        );
    }

    #[test]
    fn exit_resolver_source_facts_are_not_producer_value_or_pragma_evidence() {
        let mut ob = observation("exit", Strength::Total);
        ob.assertion_source = Some("tests/a.test.ts:7:3".into());
        ob.assertion_method = Some("equal".into());
        ob.process_exit = Some(
            serde_json::from_value(serde_json::json!({
                "model": "node-child-exit-source-v1", "status": "unresolved",
                "reason": "producer-instance-link-unverified",
                "operand": "tests/a.test.ts:30:50", "helperCalls": ["tests/a.test.ts:20:28"],
                "promise": "tests/helper.ts:50:100", "spawn": "tests/helper.ts:10:40",
                "event": {"source": "tests/helper.ts:60:90", "name": "exit"},
                "resolution": {"status": "source-checked", "source": "tests/helper.ts:75:89",
                    "field": "code", "eventArgument": "code"},
                "consumer": {"status": "source-checked", "bindings": ["tests/a.test.ts:10:28"],
                    "read": "tests/a.test.ts:30:50"}
            }))
            .unwrap(),
        );
        let encoded = serde_json::to_value(&ob).unwrap();
        assert_eq!(
            encoded["processExit"]["resolution"]["eventArgument"],
            "code"
        );
        assert_eq!(
            encoded["processExit"]["consumer"]["status"],
            "source-checked"
        );
        assert_eq!(serde_json::from_value::<Observation>(encoded).unwrap(), ob);
        let mut parent = site("parent", "return", vec![], &["T1"]);
        parent.reached.push("S1".into());
        let mut f = facts(
            vec![
                site("S1", "io-call", vec![boundary("exit")], &["T1"]),
                parent,
                site("unreached", "io-call", vec![boundary("exit")], &[]),
                site(
                    "independent",
                    "return",
                    vec![boundary("return:handler")],
                    &["T1"],
                ),
            ],
            vec![test(
                "T1",
                vec![ob.clone(), observation("return:handler", Strength::Total)],
            )],
        );
        for legacy in [false, true] {
            if legacy {
                f.tests[0].observations[0].process_exit = None;
            }
            let rows = join(&f);
            for row in &rows[..2] {
                assert_eq!(row.status, Status::Unresolved);
                assert_eq!(row.strength, None);
                assert_eq!(
                    row.reason.as_ref().unwrap().kind,
                    ReasonKind::LimitProcessExitLink
                );
            }
            assert_eq!(
                rows[2].reason.as_ref().unwrap().kind,
                ReasonKind::GapNotReached
            );
            assert_eq!(rows[3].status, Status::Evident);
            assert_eq!(
                check_pragma_hints(&f, &[pragma_hint()])[0].validation,
                HintValidation::Unresolved
            );
        }
        f.tests[0].observations.clear();
        let mut issue = rejected(WitnessIssueKind::CallNotRecorded, Some("exit"));
        issue.observation = Some(ob);
        f.tests[0].witness_issues.push(issue);
        let rows = join(&f);
        assert_eq!(
            rows[0].reason.as_ref().unwrap().kind,
            ReasonKind::LimitAssertionWitness
        );
        assert_eq!(rows[0].witness_issues.len(), 1);
    }

    #[test]
    fn mock_projections_cannot_supply_value_absence_sink_or_pragma_credit() {
        let mut direct = site("S1", "log", vec![boundary("stdout")], &["T1"]);
        direct.unmodelled_shapes = vec!["mock call identity unresolved".into()];
        let mut upstream = site("S2", "return", vec![boundary("internal")], &["T1"]);
        upstream.reached = vec!["S1".into()];
        upstream.unmodelled_shapes = direct.unmodelled_shapes.clone();
        let mut injected = site(
            "S3",
            "external-call",
            vec![boundary("callback:writer")],
            &["T1"],
        );
        injected.unmodelled_shapes = direct.unmodelled_shapes.clone();
        for kind in [
            "call-count",
            "call-arguments",
            "call-history",
            "projection",
            "future-kind",
        ] {
            let mut ob = observation("stdout", Strength::Total);
            ob.mock = Some(MockProjection {
                target: "console.log".into(),
                kind: kind.into(),
                path: vec!["mock".into(), "calls".into()],
                count_evidence: Some(serde_json::from_value(serde_json::json!({
                    "model": "node-sync-console-count-v2", "status": "source-checked",
                    "instance": "tests/a.test.ts:2:3", "createdAt": "tests/a.test.ts:2:3",
                    "readAt": "tests/a.test.ts:7:3", "installedAtRead": true,
                    "expectedCount": 1, "observedCount": 1,
                    "calls": [{"source":"src/a.ts:4:3", "action":"tests/a.test.ts:5:3", "site":"S1"}],
                    "historySelections": [{"source":"tests/a.test.ts:6:3", "inputCount":3, "from":1, "to":2}],
                    "rowBinding": {"model":"node-test-for-of-v1", "status":"source-checked", "rowIndex":0,
                        "title":"row a", "loop":"tests/a.test.ts:1:1", "table":"tests/a.test.ts:1:10",
                        "row":"tests/a.test.ts:1:12", "bindings":[{"declaration":"tests/a.test.ts:2:1", "name":"label", "value":"a"}]}
                })).unwrap()),
            });
            // Even contradictory whole-list/negative flags cannot bypass the
            // projection guard. A hint is not an escape hatch either.
            ob.call_list = true;
            ob.negative = true;
            ob.assertion_source = Some("tests/a.test.ts:7:3".into());
            ob.assertion_method = Some("equal".into());
            let mut sink_ob = ob.clone();
            sink_ob.boundary = "sink:records".into();
            let mut positive = ob.clone();
            positive.negative = false;
            let mut positive_sink = sink_ob.clone();
            positive_sink.negative = false;
            let mut t = test("T1", vec![ob.clone(), positive, sink_ob, positive_sink]);
            t.sinks.push(SinkBinding {
                sink: "sink:records".into(),
                param: "writer".into(),
                member: None,
            });
            let mut f = facts(
                vec![direct.clone(), upstream.clone(), injected.clone()],
                vec![t],
            );
            assert!(
                join(&f)
                    .iter()
                    .all(|r| r.status == Status::Unresolved && r.strength.is_none()),
                "{kind}"
            );
            assert_eq!(
                check_pragma_hints(&f, &[pragma_hint()])[0].validation,
                HintValidation::Unresolved
            );
            let joiner = Join {
                sites: f.sites.iter().map(|s| (s.id.as_str(), s)).collect(),
                tests: f.tests.iter().map(|t| (t.id.as_str(), t)).collect(),
                facts: &f,
                resolved: BTreeMap::new(),
            };
            assert!(
                !joiner.pinned(&BTreeSet::from(["T1"]), &["S1".into()]),
                "{kind}"
            );
            let bytes = serde_json::to_vec(&ob).unwrap();
            assert_eq!(serde_json::from_slice::<Observation>(&bytes).unwrap(), ob);
            f.tests[0]
                .observations
                .push(observation("stdout", Strength::Value));
            assert_eq!(
                join(&f)[0].status,
                Status::Evident,
                "independent non-mock evidence is preserved"
            );
        }
    }

    #[test]
    fn mock_projection_cannot_supply_the_early_return_decision_shortcut() {
        let mut decision = site("D1", "condition", vec![], &["T1", "T2"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            then: Some(vec!["S1".into()]),
            else_: Some(Some(vec!["S2".into()])),
            early_exit_downstream: Some(vec!["S2".into()]),
            outcomes: Some(Outcomes {
                true_: vec!["T1".into()],
                false_: vec!["T2".into()],
            }),
            ..Default::default()
        });
        let mut projected_return = observation("return:handler", Strength::Value);
        projected_return.mock = Some(MockProjection {
            target: "console.log".into(),
            kind: "projection".into(),
            path: vec!["mock".into(), "calls".into()],
            count_evidence: Some(
                serde_json::from_value(serde_json::json!({
                    "model":"node-sync-console-count-v1", "status":"source-checked",
                    "observedCount":0,"expectedCount":0,"calls":[]
                }))
                .unwrap(),
            ),
        });
        let mut f = facts(
            vec![
                site("S1", "return", vec![boundary("internal")], &["T1"]),
                site("S2", "io-call", vec![boundary("client-message")], &["T2"]),
                decision,
            ],
            vec![
                test("T1", vec![projected_return]),
                test("T2", vec![observation("client-message", Strength::Total)]),
            ],
        );
        let r = join(&f);
        let d = r.iter().find(|r| r.site == "D1").unwrap();
        assert_eq!(d.stuck_false_caught, Some(false));
        assert_eq!(d.status, Status::Partial);
        f.tests[0].observations[0].mock = None;
        let r = join(&f);
        let d = r.iter().find(|r| r.site == "D1").unwrap();
        assert_eq!(d.stuck_false_caught, Some(true));
        assert_eq!(d.status, Status::Evident);
    }

    #[test]
    fn witness_limit_kinds_and_successful_evidence_are_not_conflated() {
        for kind in [
            WitnessIssueKind::CallNotRecorded,
            WitnessIssueKind::CallIncomplete,
            WitnessIssueKind::MixedCallOutcomes,
            WitnessIssueKind::UninstrumentedObservation,
            WitnessIssueKind::CallFailed,
        ] {
            let mut f = facts(
                vec![site(
                    "S",
                    "return",
                    vec![boundary("return:handler")],
                    &["T"],
                )],
                vec![test("T", vec![])],
            );
            f.tests[0]
                .witness_issues
                .push(rejected(kind, Some("return:handler")));
            let result = join(&f);
            assert_eq!(
                result[0].reason.as_ref().unwrap().kind,
                if kind == WitnessIssueKind::CallFailed {
                    ReasonKind::GapNotAsserted
                } else {
                    ReasonKind::LimitAssertionWitness
                }
            );
            assert_eq!(result[0].witness_issues[0].issue.kind, kind);
            assert!(result[0].strength.is_none());
            f.tests[0]
                .observations
                .push(observation("return:handler", Strength::Value));
            let result = join(&f);
            assert_eq!(result[0].status, Status::Evident);
            assert_eq!(result[0].strength, Some(Strength::Value));
            assert!(result[0].reason.is_none());
        }
    }

    #[test]
    fn unrelated_tests_boundaries_and_known_execution_gaps_do_not_become_witness_limits() {
        let mut f = facts(
            vec![
                site(
                    "covered",
                    "return",
                    vec![boundary("return:handler")],
                    &["T"],
                ),
                site("uncovered", "return", vec![boundary("return:other")], &[]),
            ],
            vec![test("T", vec![]), test("foreign", vec![])],
        );
        f.tests[0].witness_issues.push(rejected(
            WitnessIssueKind::CallNotRecorded,
            Some("return:unrelated"),
        ));
        f.tests[1]
            .witness_issues
            .push(rejected(WitnessIssueKind::CaptureUnavailable, None));
        let r = join(&f);
        assert_eq!(
            r[0].reason.as_ref().unwrap().kind,
            ReasonKind::GapNotAsserted
        );
        assert_eq!(
            r[1].reason.as_ref().unwrap().kind,
            ReasonKind::GapNotReached
        );
        assert!(r.iter().all(|r| r.witness_issues.is_empty()));
    }

    #[test]
    fn decision_dependencies_retain_witness_uncertainty_without_inventing_taken_outcomes() {
        let mut d = site("D", "condition", vec![], &["T"]);
        d.kind = "decision".into();
        d.decision = Some(DecisionFacts {
            then: Some(vec!["S".into()]),
            else_: Some(None),
            outcomes: Some(Outcomes {
                true_: vec!["T".into()],
                false_: vec!["T".into()],
            }),
            ..Default::default()
        });
        let mut f = facts(
            vec![
                d,
                site("S", "return", vec![boundary("return:handler")], &["T"]),
            ],
            vec![test("T", vec![])],
        );
        f.tests[0].witness_issues.push(rejected(
            WitnessIssueKind::CallNotRecorded,
            Some("return:handler"),
        ));
        let r = join(&f);
        assert_eq!(
            r[0].reason.as_ref().unwrap().kind,
            ReasonKind::LimitAssertionWitness
        );
        assert_eq!(r[0].stuck_false_caught, Some(false));
        assert_eq!(r[0].stuck_true_caught, Some(false));
        f.sites[0]
            .decision
            .as_mut()
            .unwrap()
            .outcomes
            .as_mut()
            .unwrap()
            .false_
            .clear();
        let r = join(&f);
        assert_eq!(
            r[0].reason.as_ref().unwrap().kind,
            ReasonKind::GapOutcomeNotAsserted
        );
        assert!(!r[0].witness_issues.is_empty());
    }

    #[test]
    fn witness_provenance_crosses_cyclic_derivations_without_supplying_strength() {
        let mut s = site("S", "return", vec![boundary("return:start")], &["T"]);
        s.derive.push(Dependent {
            site: "end".into(),
            label: "flow".into(),
            strength: None,
            requires_total: None,
        });
        let mut end = site("end", "return", vec![boundary("return:end")], &["T"]);
        end.reached.push("S".into());
        let mut f = facts(vec![s, end], vec![test("T", vec![])]);
        f.tests[0].witness_issues.push(rejected(
            WitnessIssueKind::CallIncomplete,
            Some("return:end"),
        ));
        let result = join(&f);
        assert!(
            result
                .iter()
                .all(|r| r.reason.as_ref().unwrap().kind == ReasonKind::LimitAssertionWitness)
        );
        assert!(result.iter().all(|r| r.strength.is_none()));
        f.tests[0].witness_issues[0]
            .observation
            .as_mut()
            .unwrap()
            .boundary = "return:unrelated".into();
        assert!(join(&f).iter().all(|r| r.witness_issues.is_empty()));
    }

    #[test]
    fn witness_issues_round_trip_and_reject_unknown_kinds() {
        let issue = rejected(WitnessIssueKind::CallNotRecorded, Some("return:handler"));
        let json = serde_json::to_value(&issue).unwrap();
        assert_eq!(
            serde_json::from_value::<WitnessIssue>(json.clone()).unwrap(),
            issue
        );
        let mut invalid = json;
        invalid["kind"] = serde_json::json!("invented-witness");
        assert!(serde_json::from_value::<WitnessIssue>(invalid).is_err());
        let mut legacy = serde_json::to_value(test("T", vec![])).unwrap();
        legacy.as_object_mut().unwrap().remove("witnessIssues");
        assert!(
            serde_json::from_value::<TestFacts>(legacy)
                .unwrap()
                .witness_issues
                .is_empty()
        );
    }

    #[test]
    fn an_assertion_on_the_return_makes_the_return_site_evident() {
        let f = facts(
            vec![site(
                "S1",
                "return",
                vec![boundary("return:handler")],
                &["T1"],
            )],
            vec![test(
                "T1",
                vec![observation("return:handler", Strength::Total)],
            )],
        );
        let r = join(&f);
        assert_eq!(r[0].status, Status::Evident);
        assert_eq!(r[0].strength, Some(Strength::Total));
        assert!(r[0].reason.is_none());
    }

    #[test]
    fn a_site_no_test_reaches_is_a_gap_not_a_limit() {
        let f = facts(
            vec![site("S1", "return", vec![boundary("return:handler")], &[])],
            vec![],
        );
        let r = join(&f);
        assert_eq!(r[0].status, Status::Unresolved);
        assert_eq!(
            r[0].reason.as_ref().unwrap().kind,
            ReasonKind::GapNotReached
        );
    }

    #[test]
    fn an_untraceable_operand_makes_it_a_limit_rather_than_a_gap() {
        let mut s = site("S1", "return", vec![boundary("return:handler")], &["T1"]);
        s.unmodelled_shapes = vec!["blogsTried(admin) [localfn:blogsTried]".into()];
        let f = facts(vec![s], vec![test("T1", vec![])]);
        let r = join(&f);
        assert_eq!(
            r[0].reason.as_ref().unwrap().kind,
            ReasonKind::LimitOperandShape
        );
        assert!(r[0].reason.as_ref().unwrap().detail.is_some());
    }

    #[test]
    fn an_effect_with_no_boundary_is_an_internal_state_limit() {
        let f = facts(
            vec![site(
                "S1",
                "state-write",
                vec![boundary("internal")],
                &["T1"],
            )],
            vec![test(
                "T1",
                vec![observation("return:handler", Strength::Total)],
            )],
        );
        let r = join(&f);
        assert_eq!(
            r[0].reason.as_ref().unwrap().kind,
            ReasonKind::LimitInternalState
        );
    }

    #[test]
    fn a_presence_only_observation_asks_for_a_stronger_matcher() {
        let f = facts(
            vec![site(
                "S1",
                "return",
                vec![boundary("return:handler")],
                &["T1"],
            )],
            vec![test(
                "T1",
                vec![observation("return:handler", Strength::Presence)],
            )],
        );
        let r = join(&f);
        assert_eq!(r[0].status, Status::Presence);
        assert_eq!(
            r[0].reason.as_ref().unwrap().kind,
            ReasonKind::GapValueNotAsserted
        );
    }

    #[test]
    fn both_outcomes_asserted_makes_a_decision_evident_at_the_weaker_strength() {
        let mut decision = site("D1", "condition", vec![], &["T1", "T2"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            then: Some(vec!["S1".into()]),
            else_: Some(Some(vec!["S2".into()])),
            outcomes: Some(Outcomes {
                true_: vec!["T1".into()],
                false_: vec!["T2".into()],
            }),
            ..Default::default()
        });
        let f = facts(
            vec![
                site("S1", "return", vec![boundary("return:handler")], &["T1"]),
                site("S2", "return", vec![boundary("return:other")], &["T2"]),
                decision,
            ],
            vec![
                test("T1", vec![observation("return:handler", Strength::Total)]),
                test("T2", vec![observation("return:other", Strength::Value)]),
            ],
        );
        let r = join(&f);
        let d = r.iter().find(|r| r.site == "D1").unwrap();
        assert_eq!(d.status, Status::Evident);
        assert_eq!(d.strength, Some(Strength::Value));
    }

    #[test]
    fn an_outcome_no_test_takes_is_reported_as_that_gap() {
        let mut decision = site("D1", "condition", vec![], &["T1"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            then: Some(vec!["S1".into()]),
            else_: Some(Some(vec!["S2".into()])),
            outcomes: Some(Outcomes {
                true_: vec!["T1".into()],
                false_: vec![],
            }),
            ..Default::default()
        });
        let f = facts(
            vec![
                site("S1", "return", vec![boundary("return:handler")], &["T1"]),
                site("S2", "return", vec![boundary("return:other")], &[]),
                decision,
            ],
            vec![test(
                "T1",
                vec![observation("return:handler", Strength::Total)],
            )],
        );
        let r = join(&f);
        let d = r.iter().find(|r| r.site == "D1").unwrap();
        assert_eq!(d.status, Status::Partial);
        let reason = d.reason.as_ref().unwrap();
        assert_eq!(reason.kind, ReasonKind::GapOutcomeNotAsserted);
        assert_eq!(
            reason.detail.as_deref(),
            Some("no test takes the false outcome")
        );
    }

    #[test]
    fn a_total_assertion_covers_the_absence_of_an_empty_else() {
        let mut decision = site("D1", "condition", vec![], &["T1", "T2"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            then: Some(vec!["S1".into()]),
            else_: Some(None),
            outcomes: Some(Outcomes {
                true_: vec!["T1".into()],
                false_: vec!["T2".into()],
            }),
            ..Default::default()
        });
        let f = facts(
            vec![
                site("S1", "io-call", vec![boundary("client-message")], &["T1"]),
                decision,
            ],
            vec![
                test("T1", vec![observation("client-message", Strength::Total)]),
                test("T2", vec![]),
            ],
        );
        let r = join(&f);
        let d = r.iter().find(|r| r.site == "D1").unwrap();
        assert_eq!(d.status, Status::Evident);
        assert_eq!(d.absence_needed, Some(true));
    }

    #[test]
    fn an_empty_else_with_no_false_test_is_not_absence_covered() {
        let mut decision = site("D1", "condition", vec![], &["T1"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            then: Some(vec!["S1".into()]),
            else_: Some(None),
            outcomes: Some(Outcomes {
                true_: vec!["T1".into()],
                false_: vec![],
            }),
            ..Default::default()
        });
        let f = facts(
            vec![
                site("S1", "io-call", vec![boundary("client-message")], &["T1"]),
                decision,
            ],
            vec![test(
                "T1",
                vec![observation("client-message", Strength::Total)],
            )],
        );
        let r = join(&f);
        let d = r.iter().find(|r| r.site == "D1").unwrap();
        assert_eq!(d.status, Status::Partial);
    }

    #[test]
    fn a_negative_assertion_witnesses_an_early_exit() {
        let mut decision = site("D1", "condition", vec![], &["T1", "T2"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            then: Some(vec!["S1".into()]),
            else_: Some(Some(vec!["S2".into()])),
            early_exit_downstream: Some(vec!["S2".into()]),
            outcomes: Some(Outcomes {
                true_: vec!["T1".into()],
                false_: vec!["T2".into()],
            }),
            ..Default::default()
        });
        // the exit's own branch asserts nothing; the witness is T1 pinning that S2 did not happen
        let mut negative = observation("client-message", Strength::Presence);
        negative.negative = true;
        let f = facts(
            vec![
                site("S1", "return", vec![boundary("internal")], &["T1"]),
                site("S2", "io-call", vec![boundary("client-message")], &["T2"]),
                decision,
            ],
            vec![
                test("T1", vec![negative]),
                test("T2", vec![observation("client-message", Strength::Total)]),
            ],
        );
        let r = join(&f);
        let d = r.iter().find(|r| r.site == "D1").unwrap();
        assert_eq!(d.stuck_false_caught, Some(true));
        assert_eq!(d.status, Status::Evident);
    }

    #[test]
    fn a_call_list_assertion_witnesses_loop_control() {
        let mut decision = site("D1", "condition", vec![], &["T1", "T2"]);
        decision.kind = "decision".into();
        decision.decision = Some(DecisionFacts {
            then: Some(vec![]),
            else_: Some(Some(vec![])),
            loop_body: Some(vec!["S1".into()]),
            outcomes: Some(Outcomes {
                true_: vec!["T1".into()],
                false_: vec!["T2".into()],
            }),
            ..Default::default()
        });
        let mut call_list = observation("callback:admin", Strength::Value);
        call_list.call_list = true;
        let f = facts(
            vec![
                site(
                    "S1",
                    "external-call",
                    vec![boundary("callback:admin")],
                    &["T1", "T2"],
                ),
                decision,
            ],
            vec![
                test("T1", vec![call_list.clone()]),
                test("T2", vec![call_list]),
            ],
        );
        let r = join(&f);
        let d = r.iter().find(|r| r.site == "D1").unwrap();
        assert_eq!(d.status, Status::Evident);
    }

    #[test]
    fn a_dense_channel_pattern_pins_only_the_sites_it_admits() {
        let mut log_a = site("S1", "log", vec![boundary("stdout")], &["T1"]);
        log_a.method = Some("log".into());
        let mut log_b = site("S2", "log", vec![boundary("stdout")], &["T1"]);
        log_b.method = Some("log".into());
        let mut ob = observation("stdout", Strength::Value);
        ob.log_sites = Some(vec!["S1".into()]);
        let f = facts(vec![log_a, log_b], vec![test("T1", vec![ob])]);
        let r = join(&f);
        assert_eq!(r[0].status, Status::Evident);
        assert_eq!(r[1].status, Status::Unresolved);
    }

    #[test]
    fn a_pattern_several_log_sites_share_pins_none_of_them() {
        let mut log_a = site("S1", "log", vec![boundary("stdout")], &["T1"]);
        log_a.method = Some("log".into());
        let mut log_b = site("S2", "log", vec![boundary("stdout")], &["T1"]);
        log_b.method = Some("log".into());
        let mut ob = observation("stdout", Strength::Value);
        ob.log_sites = Some(vec!["S1".into(), "S2".into()]);
        ob.pattern_shared = true;
        let f = facts(vec![log_a, log_b], vec![test("T1", vec![ob])]);
        let r = join(&f);
        assert_eq!(r[0].status, Status::Unresolved);
        assert_eq!(r[1].status, Status::Unresolved);
    }

    #[test]
    fn a_timer_cancellation_candidate_requires_total_callback_evidence() {
        for (callback_strength, expected) in [
            (Strength::Presence, Status::Unresolved),
            (Strength::Value, Status::Unresolved),
            (Strength::Total, Status::Evident),
        ] {
            let mut cancel = site("cancel", "schedule", vec![boundary("internal")], &["T1"]);
            cancel.derive = vec![Dependent {
                site: "timer".into(),
                label: "cancelled timer with total sink".into(),
                strength: Some(Strength::Value),
                requires_total: Some(vec!["callback".into()]),
            }];
            let f = facts(
                vec![
                    cancel,
                    site("timer", "schedule", vec![boundary("internal")], &["T1"]),
                    site(
                        "callback",
                        "return",
                        vec![boundary("return:callback")],
                        &["T1"],
                    ),
                ],
                vec![test(
                    "T1",
                    vec![observation("return:callback", callback_strength)],
                )],
            );
            let r = join(&f);
            assert_eq!(
                r.iter().find(|r| r.site == "cancel").unwrap().status,
                expected
            );
        }
    }

    #[test]
    fn empty_or_unknown_timer_callback_candidates_do_not_supply_evidence() {
        for callbacks in [vec![], vec!["missing".into()]] {
            let mut cancel = site("cancel", "schedule", vec![boundary("internal")], &["T1"]);
            cancel.derive = vec![Dependent {
                site: "timer".into(),
                label: "cancelled timer with total sink".into(),
                strength: Some(Strength::Value),
                requires_total: Some(callbacks),
            }];
            let f = facts(
                vec![
                    cancel,
                    site("timer", "schedule", vec![boundary("internal")], &["T1"]),
                ],
                vec![test("T1", vec![])],
            );
            assert_eq!(join(&f)[0].status, Status::Unresolved);
        }
    }

    #[test]
    fn an_internal_write_is_derived_through_its_dependent_capped_at_value() {
        let mut write = site("S1", "state-write", vec![boundary("internal")], &["T1"]);
        write.derive = vec![Dependent {
            site: "S2".into(),
            label: "read this.ready".into(),
            strength: None,
            requires_total: None,
        }];
        let f = facts(
            vec![
                write,
                site("S2", "return", vec![boundary("return:handler")], &["T1"]),
            ],
            vec![test(
                "T1",
                vec![observation("return:handler", Strength::Total)],
            )],
        );
        let r = join(&f);
        let derived = r.iter().find(|r| r.site == "S1").unwrap();
        assert_eq!(derived.status, Status::Evident);
        assert_eq!(derived.strength, Some(Strength::Value));
    }

    #[test]
    fn a_dependent_no_shared_test_covers_derives_nothing() {
        let mut write = site("S1", "state-write", vec![boundary("internal")], &["T1"]);
        write.derive = vec![Dependent {
            site: "S2".into(),
            label: "read this.ready".into(),
            strength: None,
            requires_total: None,
        }];
        let f = facts(
            vec![
                write,
                site("S2", "return", vec![boundary("return:handler")], &["T2"]),
            ],
            vec![
                test("T1", vec![]),
                test("T2", vec![observation("return:handler", Strength::Total)]),
            ],
        );
        let r = join(&f);
        let derived = r.iter().find(|r| r.site == "S1").unwrap();
        assert_eq!(derived.status, Status::Unresolved);
    }

    #[test]
    fn a_weak_render_observation_marks_the_resolution_weak() {
        let mut ob = observation("dom", Strength::Presence);
        ob.weak = true;
        let f = facts(
            vec![site("S1", "return", vec![boundary("dom")], &["T1"])],
            vec![test("T1", vec![ob])],
        );
        let r = join(&f);
        assert!(r[0].weak_only);
    }

    #[test]
    fn an_explicit_null_else_survives_as_the_absence_case() {
        // A missing `else` means the decision has no branches; a null one means it has no else branch,
        // where a spurious effect is what a test would notice. The two must not collapse.
        let absence: DecisionFacts =
            serde_json::from_str(r#"{"then":["A"],"else":null}"#).expect("parses");
        assert_eq!(absence.else_, Some(None));
        let no_branches: DecisionFacts =
            serde_json::from_str(r#"{"carrier":"A"}"#).expect("parses");
        assert_eq!(no_branches.else_, None);
        let real_else: DecisionFacts =
            serde_json::from_str(r#"{"then":["A"],"else":["B"]}"#).expect("parses");
        assert_eq!(real_else.else_, Some(Some(vec!["B".to_owned()])));
    }

    #[test]
    fn the_summary_counts_limits_apart_from_gaps() {
        let mut limited = site("S1", "state-write", vec![boundary("internal")], &["T1"]);
        limited.classification = "contractual".into();
        let gap = site("S2", "return", vec![boundary("return:handler")], &[]);
        let f = facts(vec![limited, gap], vec![test("T1", vec![])]);
        let r = join(&f);
        let s = summary(&f.sites, &r);
        assert_eq!(s.contractual, 2);
        assert_eq!(s.gaps, 1);
        assert_eq!(s.limits, 1);
    }
}
