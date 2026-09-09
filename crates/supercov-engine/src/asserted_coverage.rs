//! The language-neutral join of asserted coverage: sites, evidence and observations in, a verdict per
//! site out.
//!
//! Frontends contribute facts and never verdicts, so everything syntactic happens before this module:
//! which boundaries a site's effect or value reaches, which sites each outcome of a decision controls,
//! what each assertion reads and how strongly. This module decides only what those facts imply, which is
//! the one part of the metric that is the same in every language.
//!
//! A site is **evident** when an assertion would fail if its behavior changed, **presence** when tests
//! only witness that it ran, and otherwise carries a reason: a *gap* a test can close, or an *analysis
//! limit* the frontend could not follow. A limit is never counted against a suite.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Strength
// ---------------------------------------------------------------------------

/// How completely an assertion pins what it reads. Only `Value` and `Total` prove that a changed value
/// would be noticed; `Presence` says a value arrived.
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

/// What one assertion reads, as the frontend understood it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Observation {
    pub boundary: String,
    #[serde(default)]
    pub facet: Option<String>,
    pub strength: Strength,
    #[serde(default, rename = "where")]
    pub where_: Option<String>,
    /// `expect(x).not.toHaveBeenCalled()`: the assertion pins that something did *not* happen
    #[serde(default)]
    pub negative: bool,
    /// the assertion pins a sink's whole call list, so a spurious or missing call shows up
    #[serde(default)]
    pub call_list: bool,
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
    /// Does this observation read the given boundary of the given site? Dense channels need the message
    /// constraint checked, and a spy on one console method sees only that method's calls.
    fn matches(&self, site: &Site, at: &Boundary) -> bool {
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
}

impl ReasonKind {
    pub fn is_limit(self) -> bool {
        matches!(
            self,
            ReasonKind::LimitOperandShape
                | ReasonKind::LimitInternalState
                | ReasonKind::LimitUndecidable
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_true_caught: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_false_caught: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub absence_needed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_observed: Option<bool>,
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
        .filter_map(|s| join.resolved.get(&s.id).cloned())
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
        // An effect with no boundary, reaching no other site and with no per-test extra, is internal:
        // only a derivation or a pragma can resolve it.
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
                stuck_true_caught: None,
                stuck_false_caught: None,
                absence_needed: None,
                value_observed: None,
            };
        }
        let mut best: Option<Strength> = None;
        let mut tests = BTreeSet::new();
        let mut strong_hit = false;
        for test in &covering {
            let bounds = self.bounds_for_test(site, test);
            for ob in &test.observations {
                let mut hit = bounds.iter().any(|b| ob.matches(site, b));
                // the value reached another site, and that site's boundary or sink is what the test reads
                if !hit {
                    for reached_id in &site.reached {
                        let Some(reached) = self.sites.get(reached_id.as_str()) else {
                            continue;
                        };
                        if !reached.covered_by.contains(&test.id) {
                            continue;
                        }
                        let reached_bounds = self.with_mocks(reached, test, &reached.direct_bounds);
                        if reached_bounds
                            .iter()
                            .any(|b| !b.internal() && ob.matches(reached, b))
                            || self.sink_hit(test, reached, &reached_bounds, ob)
                        {
                            hit = true;
                            break;
                        }
                    }
                }
                if !hit {
                    hit = self.sink_hit(test, site, &site.bounds, ob);
                }
                if !hit {
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
                stuck_true_caught: None,
                stuck_false_caught: None,
                absence_needed: None,
                value_observed: None,
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
            stuck_true_caught: None,
            stuck_false_caught: None,
            absence_needed: None,
            value_observed: None,
        }
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
            stuck_true_caught: stuck.then_some(false),
            stuck_false_caught: stuck.then_some(false),
            absence_needed: stuck.then_some(false),
            value_observed: None,
        };
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
                    stuck_true_caught: Some(true),
                    stuck_false_caught: Some(true),
                    absence_needed: Some(false),
                    value_observed: None,
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
                                !ob.negative
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
            stuck_true_caught: Some(stuck_true_caught),
            stuck_false_caught: Some(stuck_false_caught),
            absence_needed: Some(absence_needed),
            value_observed,
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
            stuck_true_caught: None,
            stuck_false_caught: None,
            absence_needed: None,
            value_observed: None,
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
            negative: false,
            call_list: false,
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
    fn an_internal_write_is_derived_through_its_dependent_capped_at_value() {
        let mut write = site("S1", "state-write", vec![boundary("internal")], &["T1"]);
        write.derive = vec![Dependent {
            site: "S2".into(),
            label: "read this.ready".into(),
            strength: None,
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
