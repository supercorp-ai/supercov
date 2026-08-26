#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_data_structures;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_hir_pretty;
extern crate rustc_interface;
extern crate rustc_log;
extern crate rustc_middle;
extern crate rustc_parse;
extern crate rustc_session;
extern crate rustc_span;

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process::Command,
    sync::{Mutex, OnceLock},
};

use rustc_data_structures::steal::Steal;
use rustc_driver::{Callbacks, Compilation};
use rustc_errors::ErrorGuaranteed;
use rustc_hir::{
    self as hir,
    def::DefKind,
    intravisit::{self, Visitor},
};
use rustc_interface::interface::{Compiler, Config};
use rustc_middle::{
    mir::{
        BasicBlock, BasicBlockData, Body, CallSource, LocalDecl, Operand, Place, Rvalue,
        SourceInfo, Statement, StatementKind, Terminator, TerminatorKind, UnwindAction,
        coverage::{CoverageKind, MappingKind},
        interpret::Scalar,
    },
    ty::{self, TyCtxt},
    util::Providers,
};
use rustc_session::Session;
use rustc_span::{DUMMY_SP, DesugaringKind, FileName, def_id::LocalDefId, source_map::Spanned};

use rustc_log::{
    tracing::{Event, Metadata, Subscriber, field},
    tracing_subscriber::{Layer, layer::SubscriberExt, registry::LookupSpan},
};
use rustc_parse::{lexer::StripTokens, new_parser_from_source_str};
use sha2::{Digest, Sha256};

const OUTPUT_DIRECTORY: &str = "SUPERCOV_RUSTC_SPIKE_OUTPUT";
const INSTRUMENT_MIR: &str = "SUPERCOV_RUSTC_SPIKE_INSTRUMENT_MIR";
const INSTRUMENT_CTFE: &str = "SUPERCOV_RUSTC_SPIKE_INSTRUMENT_CTFE";
const REAL_RUSTDOC: &str = "SUPERCOV_RUSTC_SPIKE_REAL_RUSTDOC";
const COMPANION_PATH: &str = "SUPERCOV_RUSTC_SPIKE_COMPANION_PATH";
const RUSTDOC_LAUNCHED: &str = "SUPERCOV_RUSTC_SPIKE_RUSTDOC_LAUNCHED";
const SOURCE_ROOT: &str = "SUPERCOV_RUSTC_SPIKE_SOURCE_ROOT";
const TARGET_ROOT: &str = "SUPERCOV_RUSTC_SPIKE_TARGET_ROOT";
const FORCE_ID_COLLISION: &str = "SUPERCOV_RUSTC_SPIKE_FORCE_ID_COLLISION";
const FORCE_PROBE_COLLISION: &str = "SUPERCOV_RUSTC_SPIKE_FORCE_PROBE_COLLISION";
const PROBE_FUNCTION: &str = "__supercov_spike_runtime::ordinal_hit";
const ENTER_CONTEXT_FUNCTION: &str = "__supercov_spike_runtime::enter_context";
const EXIT_CONTEXT_FUNCTION: &str = "__supercov_spike_runtime::exit_context";
const START_DECISION_FUNCTION: &str = "__supercov_spike_runtime::mir_decision_start";
const RECORD_CONDITION_FUNCTION: &str = "__supercov_spike_runtime::mir_decision_condition";
const FINISH_DECISION_FUNCTION: &str = "__supercov_spike_runtime::mir_decision_finish";
const START_BRANCH_FUNCTION: &str = "__supercov_spike_runtime::mir_branch_start";
const HIT_BRANCH_FUNCTION: &str = "__supercov_spike_runtime::mir_branch_hit";
const CTFE_EVENT_TARGET: &str = "rustc_const_eval::interpret::step";
const CTFE_MARKER_PREFIX: u64 = 0x5355_5045_5243_0000;
const CTFE_EDGE_MARKER_OFFSET: u64 = 0x8000;
const RUNTIME_TEMPLATE: &str =
    include_str!("../../../crates/supercov-engine/runtime-assets/rust-mmap-runtime.rs");

type OptimizedMirProvider = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Body<'tcx>;
type MirForCtfeProvider = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Body<'tcx>;
type MirBuiltProvider = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Steal<Body<'tcx>>;
type MirDropsProvider = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Steal<Body<'tcx>>;

static ORIGINAL_OPTIMIZED_MIR: OnceLock<OptimizedMirProvider> = OnceLock::new();
static ORIGINAL_MIR_FOR_CTFE: OnceLock<MirForCtfeProvider> = OnceLock::new();
static ORIGINAL_MIR_BUILT: OnceLock<MirBuiltProvider> = OnceLock::new();
static ORIGINAL_MIR_DROPS: OnceLock<MirDropsProvider> = OnceLock::new();
static CTFE_EVENTS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
static MATCH_ARM_MARKERS: Mutex<BTreeMap<String, BTreeMap<u32, u64>>> = Mutex::new(BTreeMap::new());
static MATCH_GUARD_MARKERS: Mutex<BTreeMap<String, Vec<MatchGuardConditionMarker>>> =
    Mutex::new(BTreeMap::new());
static UNREACHABLE_MATCH_ARMS: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());
static DOCTEST_ROLE: OnceLock<&'static str> = OnceLock::new();

#[derive(Clone, Debug, PartialEq, Eq)]
struct StableSourceRange {
    key: String,
    start: u32,
    end: u32,
    class: &'static str,
    owned: bool,
}

#[derive(Debug)]
struct PointObligation {
    canonical: String,
    source: StableSourceRange,
    provenance: &'static str,
    point_kind: &'static str,
    discriminator: String,
    probe_ordinal: u64,
    definitions: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct BranchAlternativeObligation {
    identity: StableObligationIdentity,
    label: &'static str,
}

#[derive(Debug)]
struct BranchObligation {
    identity: StableObligationIdentity,
    branch_kind: &'static str,
    discriminator: String,
    alternatives: Vec<BranchAlternativeObligation>,
    definitions: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct MatchArmSelectionObligation {
    branch_id: String,
    body_source: StableSourceRange,
    guarded: bool,
    guard_decision_id: Option<String>,
    selected_ordinal: u64,
    not_selected_ordinal: u64,
}

#[derive(Debug)]
struct MatchSelectionObligation {
    identity: StableObligationIdentity,
    arms: Vec<MatchArmSelectionObligation>,
    definitions: Vec<String>,
    parent_group_id: Option<String>,
    parent_site: Option<&'static str>,
    parent_arm_index: Option<usize>,
}

#[derive(Debug, PartialEq, Eq)]
struct DecisionCondition {
    source: StableSourceRange,
    branch_source: StableSourceRange,
    text: String,
    true_outcome: Option<bool>,
    false_outcome: Option<bool>,
}

#[derive(Debug)]
struct DecisionObligation {
    identity: StableObligationIdentity,
    decision_kind: &'static str,
    conditions: Vec<DecisionCondition>,
    definitions: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct MatchGuardConditionMarker {
    local: u32,
    decision_id: String,
    condition_index: usize,
}

fn prune_unreachable_match_arms(
    branches: &mut BTreeMap<String, BranchObligation>,
    match_groups: &mut BTreeMap<String, MatchSelectionObligation>,
) {
    let unreachable = UNREACHABLE_MATCH_ARMS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if unreachable.is_empty() {
        return;
    }
    let mut removed_branches = unreachable;
    match_groups.retain(|_, group| {
        group
            .arms
            .retain(|arm| !removed_branches.contains(&arm.branch_id));
        if group.arms.len() < 2 {
            removed_branches.extend(group.arms.iter().map(|arm| arm.branch_id.clone()));
            false
        } else {
            true
        }
    });
    branches.retain(|id, _| !removed_branches.contains(id));
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct StableObligationIdentity {
    id: String,
    canonical: String,
    source: StableSourceRange,
    provenance: &'static str,
    probe_ordinal: u64,
}

struct CtfeLayer;

impl<S> Layer<S> for CtfeLayer
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn enabled(
        &self,
        metadata: &Metadata<'_>,
        _context: rustc_log::tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        metadata.target() == CTFE_EVENT_TARGET
            && *metadata.level() == rustc_log::tracing::Level::INFO
    }

    fn on_event(
        &self,
        event: &Event<'_>,
        _context: rustc_log::tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = CtfeEventVisitor::default();
        event.record(&mut visitor);
        if let Some(marker) = parse_ctfe_marker(&visitor.fields) {
            CTFE_EVENTS.lock().expect("CTFE events lock").push(marker);
        }
    }
}

#[derive(Default)]
struct CtfeEventVisitor {
    fields: String,
}

impl field::Visit for CtfeEventVisitor {
    fn record_debug(&mut self, field: &field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write as _;
        let _ = write!(&mut self.fields, "{}={value:?};", field.name());
    }
}

fn normalized_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn normalized_root(variable: &str) -> Option<PathBuf> {
    env::var_os(variable)
        .map(PathBuf::from)
        .map(|path| normalized_path(&path))
}

fn generated_relative_path(path: &Path) -> Option<PathBuf> {
    let components = path.components().collect::<Vec<_>>();
    let out = components
        .iter()
        .position(|component| component.as_os_str() == "out")?;
    (out + 1 < components.len()).then(|| {
        components[out + 1..]
            .iter()
            .fold(PathBuf::new(), |mut path, component| {
                path.push(component.as_os_str());
                path
            })
    })
}

fn package_identity(crate_name: &str) -> (String, bool) {
    let Some(manifest_directory) = env::var_os("CARGO_MANIFEST_DIR")
        .map(PathBuf::from)
        .map(|path| normalized_path(&path))
    else {
        return (format!("crate:{crate_name}"), false);
    };
    let Some(source_root) = normalized_root(SOURCE_ROOT) else {
        return (format!("crate:{crate_name}"), false);
    };
    let Ok(relative) = manifest_directory.strip_prefix(source_root) else {
        return (format!("crate:{crate_name}"), false);
    };
    let package = if relative.as_os_str().is_empty() {
        ".".into()
    } else {
        relative.to_string_lossy().replace('\\', "/")
    };
    (format!("package:{package}"), true)
}

fn stable_source_range(
    tcx: TyCtxt<'_>,
    span: rustc_span::Span,
    crate_name: &str,
) -> Result<StableSourceRange, String> {
    if span.is_dummy() {
        return Err("dummy source span".into());
    }
    let source_map = tcx.sess.source_map();
    let file = source_map.lookup_source_file(span.lo());
    if !file.contains(span.hi()) {
        return Err(format!(
            "cross-file source span {}",
            source_map.span_to_diagnostic_string(span)
        ));
    }
    let start = file.original_relative_byte_pos(span.lo()).0;
    let end = file.original_relative_byte_pos(span.hi()).0;
    let (key, class, owned) = match &file.name {
        FileName::Real(name) => {
            let local_name = FileName::Real(name.clone())
                .prefer_local_unconditionally()
                .to_string_lossy()
                .into_owned();
            let path = normalized_path(Path::new(&local_name));
            if let Some(root) = normalized_root(SOURCE_ROOT)
                && let Ok(relative) = path.strip_prefix(&root)
            {
                (
                    format!("source:{}", relative.to_string_lossy().replace('\\', "/")),
                    "authored",
                    true,
                )
            } else if let Some(root) = normalized_root(TARGET_ROOT)
                && let Ok(relative) = path.strip_prefix(&root)
                && let Some(generated) = generated_relative_path(relative)
            {
                let (package, owned) = package_identity(crate_name);
                (
                    format!(
                        "generated:{package}:{}",
                        generated.to_string_lossy().replace('\\', "/")
                    ),
                    "generated",
                    owned,
                )
            } else {
                (
                    format!("external:{}", path.to_string_lossy().replace('\\', "/")),
                    "external",
                    false,
                )
            }
        }
        FileName::DocTest(path, line) => (
            format!(
                "doctest:{}:{line}",
                path.to_string_lossy().replace('\\', "/")
            ),
            "doctest",
            false,
        ),
        FileName::Custom(name) if name == "supercov-rust-runtime" => {
            ("injected:supercov-rust-runtime".into(), "injected", false)
        }
        other => (
            format!("virtual:{}", other.prefer_remapped_unconditionally()),
            "virtual",
            false,
        ),
    };
    Ok(StableSourceRange {
        key,
        start,
        end,
        class,
        owned,
    })
}

fn expansion_identity(
    tcx: TyCtxt<'_>,
    span: rustc_span::Span,
    crate_name: &str,
) -> Result<String, String> {
    let mut frames = Vec::new();
    let mut cursor = span;
    while !cursor.ctxt().is_root() {
        if frames.len() == 1024 {
            return Err("expansion chain exceeds 1024 frames".into());
        }
        let frame = cursor.ctxt().outer_expn_data();
        let callsite = stable_source_range(tcx, frame.call_site, crate_name)?;
        let definition = frame
            .macro_def_id
            .map(|def_id| tcx.def_path_str(def_id))
            .unwrap_or_else(|| "<compiler>".into());
        frames.push(format!(
            "{}\0{}\0{}\0{}\0{}",
            frame.kind.descr(),
            callsite.key,
            callsite.start,
            callsite.end,
            definition
        ));
        cursor = frame.call_site;
    }
    if frames.is_empty() {
        return Err("expanded span has no expansion backtrace".into());
    }
    Ok(frames.join("\0"))
}

fn obligation_identity(
    tcx: TyCtxt<'_>,
    def_id: rustc_span::def_id::DefId,
    span: rustc_span::Span,
    crate_name: &str,
    obligation_kind: &str,
    discriminator: &str,
    owner_local_ordinal: usize,
) -> Result<StableObligationIdentity, String> {
    let source = stable_source_range(tcx, span, crate_name)?;
    if !source.owned {
        return Err(format!("unowned {} source {}", source.class, source.key));
    }
    let callsite = stable_source_range(tcx, span.source_callsite(), crate_name)?;
    let synthetic_expansion = span.from_expansion() && source == callsite;
    let provenance = if synthetic_expansion {
        "synthetic-expansion"
    } else if span.from_expansion() {
        "authored-expansion"
    } else if source.class == "generated" {
        "generated-source"
    } else {
        "authored-source"
    };
    let canonical = if synthetic_expansion {
        let expansion = expansion_identity(tcx, span, crate_name)?;
        format!(
            "rust-source-v1\0{}\0{}\0{}\0{}\0{}\0synthetic-expansion\0{}\0{}\0{}\0",
            obligation_kind,
            source.key,
            source.start,
            source.end,
            discriminator,
            expansion,
            tcx.def_path_str(def_id),
            owner_local_ordinal,
        )
    } else {
        format!(
            "rust-source-v1\0{}\0{}\0{}\0{}\0{}\0",
            obligation_kind, source.key, source.start, source.end, discriminator,
        )
    };
    let mut hash = Sha256::new();
    hash.update(canonical.as_bytes());
    let digest = hash.finalize();
    let encoded = if env::var_os(FORCE_ID_COLLISION).is_some() {
        "000000000000000000000000".into()
    } else {
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let probe_ordinal = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 prefix"));
    Ok(StableObligationIdentity {
        id: format!("rs:{obligation_kind}:{encoded}"),
        canonical,
        source,
        provenance,
        probe_ordinal: if env::var_os(FORCE_ID_COLLISION).is_some()
            || env::var_os(FORCE_PROBE_COLLISION).is_some()
        {
            0
        } else {
            probe_ordinal
        },
    })
}

fn function_identity(
    tcx: TyCtxt<'_>,
    def_id: rustc_span::def_id::DefId,
    span: rustc_span::Span,
    crate_name: &str,
) -> Result<StableObligationIdentity, String> {
    obligation_identity(tcx, def_id, span, crate_name, "function", "", 0)
}

fn is_function_body(kind: DefKind) -> bool {
    matches!(
        kind,
        DefKind::Fn | DefKind::AssocFn | DefKind::Closure | DefKind::SyntheticCoroutineBody
    )
}

struct HirManifestCollector<'a, 'tcx> {
    tcx: TyCtxt<'tcx>,
    def_id: rustc_span::def_id::DefId,
    crate_name: &'a str,
    definition: String,
    ordinal: usize,
    points: &'a mut BTreeMap<String, PointObligation>,
    branches: &'a mut BTreeMap<String, BranchObligation>,
    decisions: &'a mut BTreeMap<String, DecisionObligation>,
    match_groups: &'a mut BTreeMap<String, MatchSelectionObligation>,
    limitations: &'a mut BTreeSet<String>,
    control_overrides: BTreeMap<u32, &'static str>,
    match_context: Option<(String, &'static str, Option<usize>)>,
}

impl<'a, 'tcx> HirManifestCollector<'a, 'tcx> {
    fn identity(
        &mut self,
        kind: &str,
        span: rustc_span::Span,
        discriminator: &str,
    ) -> Option<StableObligationIdentity> {
        if let Ok(source) = stable_source_range(self.tcx, span, self.crate_name)
            && !source.owned
            && span.from_expansion()
            && stable_source_range(self.tcx, span.source_callsite(), self.crate_name)
                .is_ok_and(|callsite| callsite.owned)
        {
            // The external declarative macro's implementation is not part of
            // the owned source graph. Synthetic proc-macro output instead
            // carries the owned invocation span and reaches the normal path.
            return None;
        }
        let ordinal = self.ordinal;
        self.ordinal += 1;
        match obligation_identity(
            self.tcx,
            self.def_id,
            span,
            self.crate_name,
            kind,
            discriminator,
            ordinal,
        ) {
            Ok(identity) => Some(identity),
            Err(error) => {
                self.limitations.insert(format!(
                    "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: {kind}: {error}",
                    self.definition
                ));
                None
            }
        }
    }

    fn point(&mut self, span: rustc_span::Span, point_kind: &'static str, discriminator: &str) {
        if span.desugaring_kind().is_some() {
            // Compiler lowering scaffolding is not authored executable source.
            // The enclosing control construct records its explicit obligations
            // from the source callsite instead.
            return;
        }
        let Some(identity) = self.identity(point_kind, span, discriminator) else {
            return;
        };
        match self.points.get_mut(&identity.id) {
            Some(existing) if existing.canonical != identity.canonical => self.tcx.dcx().fatal(
                format!("Supercov Rust obligation ID collision for {}", identity.id),
            ),
            Some(existing) => {
                existing.definitions.push(self.definition.clone());
                existing.definitions.sort();
                existing.definitions.dedup();
            }
            None => {
                self.points.insert(
                    identity.id.clone(),
                    PointObligation {
                        canonical: identity.canonical,
                        source: identity.source,
                        provenance: identity.provenance,
                        point_kind,
                        discriminator: discriminator.into(),
                        probe_ordinal: identity.probe_ordinal,
                        definitions: vec![self.definition.clone()],
                    },
                );
            }
        }
    }

    fn record_control_decision(
        &mut self,
        expression: &'tcx hir::Expr<'tcx>,
        condition: &'tcx hir::Expr<'tcx>,
        control_kind: &'static str,
    ) -> Option<String> {
        let mut atomic = Vec::new();
        flatten_decision_expression(condition, Some(true), Some(false), &mut atomic);
        if atomic.iter().any(|condition| {
            let span = condition.expression.span;
            span.from_expansion()
                && !self.tcx.def_span(self.def_id).from_expansion()
                && !span
                    .ctxt()
                    .outer_expn_data()
                    .macro_def_id
                    .is_some_and(|macro_def| macro_def.is_local())
        }) {
            // Hidden control flow emitted by an external macro (for example
            // assert! or println!) is implementation code of that macro, not
            // an authored decision in the caller's denominator.
            return None;
        }
        let has_let = atomic
            .iter()
            .any(|condition| matches!(condition.expression.kind, hir::ExprKind::Let(_)));
        let decision_kind = match (control_kind, has_let, atomic.len() > 1) {
            ("if", false, _) => "if",
            ("if", true, false) => "if-let",
            ("if", true, true) => "let-chain",
            ("while", false, _) => "while",
            ("while", true, _) => "while-let",
            ("match-guard", _, _) => "match-guard",
            _ => self.tcx.dcx().fatal(format!(
                "Supercov has no Rust decision kind for {control_kind} in {}",
                self.definition
            )),
        };
        let mut conditions = Vec::with_capacity(atomic.len());
        for condition in atomic {
            let source =
                match stable_source_range(self.tcx, condition.expression.span, self.crate_name) {
                    Ok(source) if source.owned => source,
                    Ok(source) => {
                        self.limitations.insert(format!(
                            "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: condition: unowned {} source {}",
                            self.definition, source.class, source.key
                        ));
                        return None;
                    }
                    Err(error) => {
                        self.limitations.insert(format!(
                            "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: condition: {error}",
                            self.definition
                        ));
                        return None;
                    }
                };
            let branch_source = match stable_source_range(
                self.tcx,
                match condition.expression.kind {
                    hir::ExprKind::Let(let_expression) => let_expression.pat.span,
                    _ => condition.expression.span,
                },
                self.crate_name,
            ) {
                Ok(branch_source) if branch_source.owned => branch_source,
                Ok(branch_source) => {
                    self.limitations.insert(format!(
                        "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: branch condition: unowned {} source {}",
                        self.definition, branch_source.class, branch_source.key
                    ));
                    return None;
                }
                Err(error) => {
                    self.limitations.insert(format!(
                        "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: branch condition: {error}",
                        self.definition
                    ));
                    return None;
                }
            };
            conditions.push(DecisionCondition {
                branch_source,
                text: if condition.expression.span.from_expansion()
                    && stable_source_range(
                        self.tcx,
                        condition.expression.span.source_callsite(),
                        self.crate_name,
                    )
                    .is_ok_and(|callsite| callsite == source)
                {
                    rustc_hir_pretty::expr_to_string(&self.tcx, condition.expression)
                } else {
                    self.tcx
                        .sess
                        .source_map()
                        .span_to_snippet(condition.expression.span)
                        .unwrap_or_else(|_| "<source unavailable>".into())
                },
                source,
                true_outcome: condition.true_outcome,
                false_outcome: condition.false_outcome,
            });
        }
        let decision = self.identity("decision", condition.span, decision_kind)?;
        let decision_id = decision.id.clone();
        match self.decisions.get_mut(&decision.id) {
            Some(existing) if existing.identity.canonical != decision.canonical => {
                self.tcx.dcx().fatal(format!(
                    "Supercov Rust obligation ID collision for {}",
                    decision.id
                ))
            }
            Some(existing)
                if existing.decision_kind != decision_kind || existing.conditions != conditions =>
            {
                self.tcx.dcx().fatal(format!(
                    "Supercov Rust decision aggregation mismatch for {}",
                    decision.id
                ))
            }
            Some(existing) => {
                existing.definitions.push(self.definition.clone());
                existing.definitions.sort();
                existing.definitions.dedup();
            }
            None => {
                self.decisions.insert(
                    decision.id.clone(),
                    DecisionObligation {
                        identity: decision,
                        decision_kind,
                        conditions,
                        definitions: vec![self.definition.clone()],
                    },
                );
            }
        }

        let decision_span = if control_kind == "while" {
            expression.span.source_callsite()
        } else {
            expression.span
        };
        let _ = self.record_branch(
            decision_span,
            "decision-outcome",
            &format!("decision-outcome:{decision_kind}"),
            &[("true", "condition true"), ("false", "condition false")],
        );
        Some(decision_id)
    }

    fn record_branch(
        &mut self,
        span: rustc_span::Span,
        branch_kind: &'static str,
        discriminator: &str,
        alternatives: &[(&str, &'static str)],
    ) -> Option<String> {
        let branch = self.identity("branch", span, discriminator)?;
        let alternatives = alternatives
            .iter()
            .filter_map(|(alternative, label)| {
                self.identity(
                    "branch-alternative",
                    span,
                    &format!("{discriminator}:{alternative}"),
                )
                .map(|identity| BranchAlternativeObligation { identity, label })
            })
            .collect::<Vec<_>>();
        if alternatives.len() != 2 {
            return None;
        }
        let branch_id = branch.id.clone();
        match self.branches.get_mut(&branch.id) {
            Some(existing) if existing.identity.canonical != branch.canonical => {
                self.tcx.dcx().fatal(format!(
                    "Supercov Rust obligation ID collision for {}",
                    branch.id
                ))
            }
            Some(existing)
                if existing.branch_kind != branch_kind
                    || existing.discriminator != discriminator
                    || existing.alternatives != alternatives =>
            {
                self.tcx.dcx().fatal(format!(
                    "Supercov Rust branch aggregation mismatch for {}",
                    branch.id
                ))
            }
            Some(existing) => {
                existing.definitions.push(self.definition.clone());
                existing.definitions.sort();
                existing.definitions.dedup();
            }
            None => {
                self.branches.insert(
                    branch.id.clone(),
                    BranchObligation {
                        identity: branch,
                        branch_kind,
                        discriminator: discriminator.into(),
                        alternatives,
                        definitions: vec![self.definition.clone()],
                    },
                );
            }
        }
        Some(branch_id)
    }

    fn record_match(
        &mut self,
        expression: &'tcx hir::Expr<'tcx>,
        arms: &'tcx [hir::Arm<'tcx>],
    ) -> Option<String> {
        if arms.len() < 2 {
            // An irrefutable single-arm match has no selectable alternative;
            // an empty match diverges while evaluating an uninhabited value.
            return None;
        }
        if expression.span.from_expansion()
            && !self.tcx.def_span(self.def_id).from_expansion()
            && !expression
                .span
                .ctxt()
                .outer_expn_data()
                .macro_def_id
                .is_some_and(|macro_def| macro_def.is_local())
        {
            return None;
        }
        let group = self.identity("match-group", expression.span, "match")?;
        let mut selections = Vec::with_capacity(arms.len());
        for (index, arm) in arms.iter().enumerate() {
            let discriminator = format!("match-arm:{}:{index}", group.id);
            let branch_id = self.record_branch(
                arm.span,
                "match-arm",
                &discriminator,
                &[("not-selected", "not selected"), ("selected", "selected")],
            )?;
            let Some(branch) = self.branches.get(&branch_id) else {
                self.tcx
                    .dcx()
                    .fatal(format!("Supercov lost Rust match-arm branch {branch_id}"));
            };
            let body_source = match stable_source_range(self.tcx, arm.body.span, self.crate_name) {
                Ok(source) if source.owned => source,
                Ok(source) => {
                    self.limitations.insert(format!(
                        "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: match arm body: unowned {} source {}",
                        self.definition, source.class, source.key
                    ));
                    return None;
                }
                Err(error) => {
                    self.limitations.insert(format!(
                        "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: match arm body: {error}",
                        self.definition
                    ));
                    return None;
                }
            };
            let not_selected_ordinal = branch
                .alternatives
                .iter()
                .find(|alternative| alternative.label == "not selected")
                .map(|alternative| alternative.identity.probe_ordinal);
            let selected_ordinal = branch
                .alternatives
                .iter()
                .find(|alternative| alternative.label == "selected")
                .map(|alternative| alternative.identity.probe_ordinal);
            let (Some(not_selected_ordinal), Some(selected_ordinal)) =
                (not_selected_ordinal, selected_ordinal)
            else {
                self.tcx.dcx().fatal(format!(
                    "Supercov match-arm branch {branch_id} has incomplete alternatives"
                ));
            };
            selections.push(MatchArmSelectionObligation {
                branch_id,
                body_source,
                guarded: arm.guard.is_some(),
                guard_decision_id: None,
                selected_ordinal,
                not_selected_ordinal,
            });
        }
        for (selection, arm) in selections.iter_mut().zip(arms) {
            selection.guard_decision_id = arm
                .guard
                .and_then(|guard| self.record_control_decision(guard, guard, "match-guard"));
        }
        let group_id = group.id.clone();
        let parent = self.match_context.clone();
        match self.match_groups.get_mut(&group.id) {
            Some(existing) if existing.identity.canonical != group.canonical => {
                self.tcx.dcx().fatal(format!(
                    "Supercov Rust obligation ID collision for {}",
                    group.id
                ))
            }
            Some(existing)
                if existing.arms != selections
                    || existing.parent_group_id != parent.as_ref().map(|value| value.0.clone())
                    || existing.parent_site != parent.as_ref().map(|value| value.1)
                    || existing.parent_arm_index != parent.as_ref().and_then(|value| value.2) =>
            {
                self.tcx.dcx().fatal(format!(
                    "Supercov Rust match selection aggregation mismatch for {}",
                    group.id
                ))
            }
            Some(existing) => {
                existing.definitions.push(self.definition.clone());
                existing.definitions.sort();
                existing.definitions.dedup();
            }
            None => {
                self.match_groups.insert(
                    group.id.clone(),
                    MatchSelectionObligation {
                        identity: group,
                        arms: selections,
                        definitions: vec![self.definition.clone()],
                        parent_group_id: parent.as_ref().map(|value| value.0.clone()),
                        parent_site: parent.as_ref().map(|value| value.1),
                        parent_arm_index: parent.as_ref().and_then(|value| value.2),
                    },
                );
            }
        }
        Some(group_id)
    }
}

struct AtomicDecisionExpression<'tcx> {
    expression: &'tcx hir::Expr<'tcx>,
    true_outcome: Option<bool>,
    false_outcome: Option<bool>,
}

fn flatten_decision_expression<'tcx>(
    expression: &'tcx hir::Expr<'tcx>,
    true_outcome: Option<bool>,
    false_outcome: Option<bool>,
    output: &mut Vec<AtomicDecisionExpression<'tcx>>,
) {
    match expression.kind {
        hir::ExprKind::Binary(operator, left, right) => match operator.node {
            rustc_ast::BinOpKind::And => {
                flatten_decision_expression(left, None, false_outcome, output);
                flatten_decision_expression(right, true_outcome, false_outcome, output);
            }
            rustc_ast::BinOpKind::Or => {
                flatten_decision_expression(left, true_outcome, None, output);
                flatten_decision_expression(right, true_outcome, false_outcome, output);
            }
            _ => output.push(AtomicDecisionExpression {
                expression,
                true_outcome,
                false_outcome,
            }),
        },
        _ => output.push(AtomicDecisionExpression {
            expression,
            true_outcome,
            false_outcome,
        }),
    }
}

impl<'tcx> Visitor<'tcx> for HirManifestCollector<'_, 'tcx> {
    fn visit_stmt(&mut self, statement: &'tcx hir::Stmt<'tcx>) {
        match statement.kind {
            hir::StmtKind::Let(local) if local.init.is_some() => {
                self.point(statement.span, "statement", "let")
            }
            hir::StmtKind::Expr(_) | hir::StmtKind::Semi(_) => {
                self.point(statement.span, "statement", "expression")
            }
            hir::StmtKind::Let(_) | hir::StmtKind::Item(_) => {}
        }
        intravisit::walk_stmt(self, statement);
    }

    fn visit_block(&mut self, block: &'tcx hir::Block<'tcx>) {
        if let Some(tail) = block.expr {
            self.point(tail.span, "statement", "tail-expression");
        }
        intravisit::walk_block(self, block);
    }

    fn visit_expr(&mut self, expression: &'tcx hir::Expr<'tcx>) {
        if let hir::ExprKind::Loop(block, _, source, _) = expression.kind {
            match source {
                hir::LoopSource::While => {
                    if let Some(control) = block.expr
                        && matches!(control.kind, hir::ExprKind::If(_, _, _))
                    {
                        self.control_overrides
                            .insert(control.hir_id.local_id.as_u32(), "while");
                        let _ = self.record_branch(
                            expression.span.source_callsite(),
                            "loop-entry",
                            "loop-entry:while",
                            &[("zero", "zero iterations"), ("entered", "entered")],
                        );
                    } else {
                        self.limitations.insert(format!(
                            "RUST_CONTROL_MAPPING_UNRESOLVED: {}: while control is not the expected expanded HIR shape",
                            self.definition
                        ));
                    }
                }
                hir::LoopSource::ForLoop => {
                    let _ = self.record_branch(
                        expression.span.source_callsite(),
                        "loop-entry",
                        "loop-entry:for",
                        &[("zero", "zero iterations"), ("entered", "entered")],
                    );
                }
                hir::LoopSource::Loop => {}
            }
        }
        if let hir::ExprKind::Match(scrutinee, arms, source) = expression.kind
            && matches!(source, hir::MatchSource::Normal | hir::MatchSource::Postfix)
            && let Some(group_id) = self.record_match(expression, arms)
        {
            let previous = self
                .match_context
                .replace((group_id.clone(), "scrutinee", None));
            self.visit_expr(scrutinee);
            for (index, arm) in arms.iter().enumerate() {
                self.visit_pat(arm.pat);
                if let Some(guard) = arm.guard {
                    self.match_context = Some((group_id.clone(), "guard", Some(index)));
                    self.visit_expr(guard);
                }
                self.match_context = Some((group_id.clone(), "body", Some(index)));
                self.visit_expr(arm.body);
            }
            self.match_context = previous;
            return;
        }
        if let hir::ExprKind::If(condition, _, _) = expression.kind {
            let control_kind = self
                .control_overrides
                .remove(&expression.hir_id.local_id.as_u32())
                .unwrap_or("if");
            let _ = self.record_control_decision(expression, condition, control_kind);
        }
        intravisit::walk_expr(self, expression);
    }
}

fn manifest_json(
    crate_name: &str,
    points: &BTreeMap<String, PointObligation>,
    branches: &BTreeMap<String, BranchObligation>,
    decisions: &BTreeMap<String, DecisionObligation>,
    match_groups: &BTreeMap<String, MatchSelectionObligation>,
    limitations: &[String],
) -> String {
    let points = points
        .iter()
        .map(|(id, obligation)| {
            format!(
                "{{\"id\":\"{}\",\"kind\":\"{}\",\"sourceKey\":\"{}\",\"start\":{},\"end\":{},\"provenance\":\"{}\",\"discriminator\":\"{}\",\"probeOrdinal\":\"{}\",\"definitions\":{},\"canonical\":\"{}\"}}",
                escape(id),
                obligation.point_kind,
                escape(&obligation.source.key),
                obligation.source.start,
                obligation.source.end,
                obligation.provenance,
                escape(&obligation.discriminator),
                obligation.probe_ordinal,
                json_strings(&obligation.definitions),
                escape(&obligation.canonical),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let branches = branches
        .values()
        .map(|branch| {
            let alternatives = branch
                .alternatives
                .iter()
                .map(|alternative| {
                    format!(
                        "{{\"id\":\"{}\",\"label\":\"{}\",\"probeOrdinal\":\"{}\"}}",
                        escape(&alternative.identity.id),
                        alternative.label,
                        alternative.identity.probe_ordinal,
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"id\":\"{}\",\"kind\":\"{}\",\"discriminator\":\"{}\",\"sourceKey\":\"{}\",\"start\":{},\"end\":{},\"provenance\":\"{}\",\"probeOrdinal\":\"{}\",\"definitions\":{},\"alternatives\":[{}],\"canonical\":\"{}\"}}",
                escape(&branch.identity.id),
                branch.branch_kind,
                escape(&branch.discriminator),
                escape(&branch.identity.source.key),
                branch.identity.source.start,
                branch.identity.source.end,
                branch.identity.provenance,
                branch.identity.probe_ordinal,
                json_strings(&branch.definitions),
                alternatives,
                escape(&branch.identity.canonical),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let decisions = decisions
        .values()
        .map(|decision| {
            let conditions = decision
                .conditions
                .iter()
                .map(|condition| {
                    format!(
                        "{{\"sourceKey\":\"{}\",\"start\":{},\"end\":{},\"source\":\"{}\"}}",
                        escape(&condition.source.key),
                        condition.source.start,
                        condition.source.end,
                        escape(&condition.text),
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"id\":\"{}\",\"kind\":\"{}\",\"sourceKey\":\"{}\",\"start\":{},\"end\":{},\"provenance\":\"{}\",\"probeOrdinal\":\"{}\",\"definitions\":{},\"conditions\":[{}],\"canonical\":\"{}\"}}",
                escape(&decision.identity.id),
                decision.decision_kind,
                escape(&decision.identity.source.key),
                decision.identity.source.start,
                decision.identity.source.end,
                decision.identity.provenance,
                decision.identity.probe_ordinal,
                json_strings(&decision.definitions),
                conditions,
                escape(&decision.identity.canonical),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let selection_groups = match_groups
        .values()
        .map(|group| {
            let arms = group
                .arms
                .iter()
                .map(|arm| {
                    format!(
                        "{{\"branchId\":\"{}\",\"bodySourceKey\":\"{}\",\"bodyStart\":{},\"bodyEnd\":{},\"guarded\":{},\"guardDecisionId\":{},\"selectedOrdinal\":\"{}\",\"notSelectedOrdinal\":\"{}\"}}",
                        escape(&arm.branch_id),
                        escape(&arm.body_source.key),
                        arm.body_source.start,
                        arm.body_source.end,
                        arm.guarded,
                        json_string(arm.guard_decision_id.as_deref()),
                        arm.selected_ordinal,
                        arm.not_selected_ordinal,
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!(
                "{{\"id\":\"{}\",\"kind\":\"match\",\"sourceKey\":\"{}\",\"start\":{},\"end\":{},\"provenance\":\"{}\",\"probeOrdinal\":\"{}\",\"definitions\":{},\"parentGroupId\":{},\"parentSite\":{},\"parentArmIndex\":{},\"arms\":[{}],\"canonical\":\"{}\"}}",
                escape(&group.identity.id),
                escape(&group.identity.source.key),
                group.identity.source.start,
                group.identity.source.end,
                group.identity.provenance,
                group.identity.probe_ordinal,
                json_strings(&group.definitions),
                json_string(group.parent_group_id.as_deref()),
                json_string(group.parent_site),
                group
                    .parent_arm_index
                    .map(|index| index.to_string())
                    .unwrap_or_else(|| "null".into()),
                arms,
                escape(&group.identity.canonical),
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let limitations = limitations
        .iter()
        .map(|limitation| format!("\"{}\"", escape(limitation)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":\"supercov-rust-manifest-candidate-v1\",\"model\":\"rust-source-v1\",\"crate\":\"{}\",\"measurementComplete\":false,\"points\":[{}],\"branches\":[{}],\"decisions\":[{}],\"selectionGroups\":[{}],\"limitations\":[{}]}}\n",
        escape(crate_name),
        points,
        branches,
        decisions,
        selection_groups,
        limitations
    )
}

fn reject_probe_ordinal_collisions(
    tcx: TyCtxt<'_>,
    points: &BTreeMap<String, PointObligation>,
    branches: &BTreeMap<String, BranchObligation>,
    decisions: &BTreeMap<String, DecisionObligation>,
    match_groups: &BTreeMap<String, MatchSelectionObligation>,
) {
    let mut ordinals = BTreeMap::<u64, String>::new();
    let mut insert = |ordinal: u64, id: &str| {
        if let Some(existing) = ordinals.insert(ordinal, id.into())
            && existing != id
        {
            tcx.dcx().fatal(format!(
                "Supercov Rust probe ordinal collision between {existing} and {id}"
            ));
        }
    };
    for (id, point) in points {
        insert(point.probe_ordinal, id);
    }
    for branch in branches.values() {
        insert(branch.identity.probe_ordinal, &branch.identity.id);
        for alternative in &branch.alternatives {
            insert(alternative.identity.probe_ordinal, &alternative.identity.id);
        }
    }
    for decision in decisions.values() {
        insert(decision.identity.probe_ordinal, &decision.identity.id);
    }
    for group in match_groups.values() {
        insert(group.identity.probe_ordinal, &group.identity.id);
    }
}

fn parse_ctfe_marker(fields: &str) -> Option<u64> {
    let marker = fields
        .split_once("const ")?
        .1
        .split_once("_u64")?
        .0
        .parse::<u64>()
        .ok()?;
    (marker & !0xffff == CTFE_MARKER_PREFIX).then_some(marker)
}

struct ProbeCallbacks;

impl Callbacks for ProbeCallbacks {
    fn config(&mut self, config: &mut Config) {
        config.override_queries = Some(install_query_overrides);
    }

    fn after_crate_root_parsing(
        &mut self,
        compiler: &Compiler,
        krate: &mut rustc_ast::Crate,
    ) -> Compilation {
        if env::var_os(INSTRUMENT_MIR).is_none() {
            return Compilation::Continue;
        }
        let runtime = RUNTIME_TEMPLATE.replace("__SUPERCOV_MODULE__", "__supercov_spike_runtime");
        let mut parser = rustc_parse::unwrap_or_emit_fatal(new_parser_from_source_str(
            &compiler.sess.psess,
            FileName::Custom("<supercov-rust-runtime>".into()),
            runtime,
            StripTokens::Nothing,
        ));
        let injected = parser.parse_crate_mod().unwrap_or_else(|error| {
            let guaranteed: ErrorGuaranteed = error.emit();
            guaranteed.raise_fatal()
        });
        krate.items.extend(injected.items);
        Compilation::Continue
    }

    fn after_analysis<'tcx>(&mut self, _compiler: &Compiler, tcx: TyCtxt<'tcx>) -> Compilation {
        let Ok(directory) = env::var(OUTPUT_DIRECTORY) else {
            return Compilation::Continue;
        };
        let directory = PathBuf::from(directory);
        if let Err(error) = fs::create_dir_all(&directory) {
            tcx.dcx().fatal(format!(
                "Supercov could not create the Rust compiler output directory: {error}"
            ));
        }

        let crate_name = tcx.crate_name(rustc_span::def_id::LOCAL_CRATE);
        let crate_name_string = crate_name.to_string();
        let output = directory.join(format!(
            "{}-{}.jsonl",
            std::process::id(),
            sanitize(&crate_name_string)
        ));
        let Ok(mut output) = OpenOptions::new().create_new(true).write(true).open(output) else {
            tcx.dcx()
                .fatal("Supercov could not create a unique Rust compiler trace");
        };
        let source_map = tcx.sess.source_map();
        let doctest_role = DOCTEST_ROLE.get().copied();
        let doctest_path = env::var("UNSTABLE_RUSTDOC_TEST_PATH").ok();
        let doctest_line = env::var("UNSTABLE_RUSTDOC_TEST_LINE").ok();
        let mut points = BTreeMap::<String, PointObligation>::new();
        let mut branches = BTreeMap::<String, BranchObligation>::new();
        let mut decisions = BTreeMap::<String, DecisionObligation>::new();
        let mut match_groups = BTreeMap::<String, MatchSelectionObligation>::new();
        let mut manifest_limitations = BTreeSet::from([
            "RUST_MANIFEST_CANDIDATE_REMAINING_SURFACES: let-else, try, assertion, CTFE and doctest obligation/probe mappings are not emitted yet".to_owned(),
        ]);

        for owner in tcx.hir_body_owners() {
            let def_id = owner.to_def_id();
            let span = tcx.def_span(def_id);
            let callsite = span.source_callsite();
            let kind = tcx.def_kind(def_id);
            let mir = if matches!(
                kind,
                DefKind::Const
                    | DefKind::AssocConst
                    | DefKind::Static { .. }
                    | DefKind::AnonConst
                    | DefKind::InlineConst
            ) {
                tcx.mir_for_ctfe(def_id)
            } else {
                tcx.optimized_mir(def_id)
            };
            let mut mir_spans = mir
                .basic_blocks
                .iter()
                .flat_map(|block| {
                    block
                        .statements
                        .iter()
                        .map(|statement| statement.source_info.span)
                        .chain(
                            block
                                .terminator
                                .iter()
                                .map(|terminator| terminator.source_info.span),
                        )
                })
                .map(|span| source_map.span_to_diagnostic_string(span))
                .collect::<Vec<_>>();
            mir_spans.sort();
            mir_spans.dedup();
            let mut mir_authored_lines = if doctest_role == Some("standalone") {
                mir.basic_blocks
                    .iter()
                    .flat_map(|block| {
                        block
                            .statements
                            .iter()
                            .map(|statement| statement.source_info.span)
                            .chain(
                                block
                                    .terminator
                                    .iter()
                                    .map(|terminator| terminator.source_info.span),
                            )
                    })
                    .map(|span| source_map.lookup_char_pos(span.lo()))
                    .map(|location| {
                        source_map.doctest_offset_line(&location.file.name, location.line)
                    })
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            mir_authored_lines.sort();
            mir_authored_lines.dedup();
            let source_snippet = source_map.span_to_snippet(span).ok();
            let body_snippet = source_map
                .span_to_snippet(tcx.hir_body_owned_by(owner).value.span)
                .ok();
            let definition = tcx.def_path_str(def_id);
            let owned_body = !definition.contains("__supercov_spike_runtime");
            let function_identity = if is_function_body(kind) && owned_body {
                match function_identity(tcx, def_id, span, &crate_name_string) {
                    Ok(identity) => {
                        match points.get_mut(&identity.id) {
                            Some(existing) if existing.canonical != identity.canonical => {
                                tcx.dcx().fatal(format!(
                                    "Supercov Rust obligation ID collision for {}",
                                    identity.id
                                ))
                            }
                            Some(existing) => {
                                existing.definitions.push(tcx.def_path_str(def_id));
                                existing.definitions.sort();
                                existing.definitions.dedup();
                            }
                            None => {
                                points.insert(
                                    identity.id.clone(),
                                    PointObligation {
                                        canonical: identity.canonical.clone(),
                                        source: identity.source.clone(),
                                        provenance: identity.provenance,
                                        point_kind: "function",
                                        discriminator: String::new(),
                                        probe_ordinal: identity.probe_ordinal,
                                        definitions: vec![tcx.def_path_str(def_id)],
                                    },
                                );
                            }
                        }
                        Some(identity)
                    }
                    Err(error) => {
                        manifest_limitations.insert(format!(
                            "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: {error}",
                            tcx.def_path_str(def_id)
                        ));
                        None
                    }
                }
            } else {
                None
            };
            if owned_body {
                let body = tcx.hir_body_owned_by(owner);
                let mut collector = HirManifestCollector {
                    tcx,
                    def_id,
                    crate_name: &crate_name_string,
                    definition: definition.clone(),
                    ordinal: 1,
                    points: &mut points,
                    branches: &mut branches,
                    decisions: &mut decisions,
                    match_groups: &mut match_groups,
                    limitations: &mut manifest_limitations,
                    control_overrides: BTreeMap::new(),
                    match_context: None,
                };
                collector.visit_body(body);
            }
            let record = format!(
                "{{\"crate\":\"{}\",\"definition\":\"{}\",\"kind\":\"{:?}\",\"span\":\"{}\",\"callsite\":\"{}\",\"expanded\":{},\"mirBlocks\":{},\"mirSpans\":{},\"mirAuthoredLines\":{},\"sourceSnippet\":{},\"bodySnippet\":{},\"doctestRole\":{},\"doctestPath\":{},\"doctestLine\":{},\"functionObligationId\":{},\"sourceKey\":{},\"sourceStart\":{},\"sourceEnd\":{},\"sourceProvenance\":{}}}\n",
                escape(&crate_name_string),
                escape(&tcx.def_path_str(def_id)),
                kind,
                escape(&source_map.span_to_diagnostic_string(span)),
                escape(&source_map.span_to_diagnostic_string(callsite)),
                span.from_expansion(),
                mir.basic_blocks.len(),
                json_strings(&mir_spans),
                json_usizes(&mir_authored_lines),
                json_string(source_snippet.as_deref()),
                json_string(body_snippet.as_deref()),
                json_string(doctest_role),
                json_string(doctest_path.as_deref()),
                json_string(doctest_line.as_deref()),
                json_string(
                    function_identity
                        .as_ref()
                        .map(|identity| identity.id.as_str())
                ),
                json_string(
                    function_identity
                        .as_ref()
                        .map(|identity| identity.source.key.as_str()),
                ),
                function_identity.as_ref().map_or_else(
                    || "null".into(),
                    |identity| identity.source.start.to_string(),
                ),
                function_identity
                    .as_ref()
                    .map_or_else(|| "null".into(), |identity| identity.source.end.to_string(),),
                json_string(
                    function_identity
                        .as_ref()
                        .map(|identity| identity.provenance),
                ),
            );
            if output.write_all(record.as_bytes()).is_err() {
                tcx.dcx()
                    .fatal("Supercov could not persist the Rust compiler trace");
            }
        }
        if output.flush().is_err() {
            tcx.dcx()
                .fatal("Supercov could not flush the Rust compiler trace");
        }
        let manifest_path = directory.join(format!(
            "manifest-{}-{}.json",
            std::process::id(),
            sanitize(&crate_name_string)
        ));
        prune_unreachable_match_arms(&mut branches, &mut match_groups);
        let limitations = manifest_limitations.into_iter().collect::<Vec<_>>();
        reject_probe_ordinal_collisions(tcx, &points, &branches, &decisions, &match_groups);
        let manifest = manifest_json(
            &crate_name_string,
            &points,
            &branches,
            &decisions,
            &match_groups,
            &limitations,
        );
        let Ok(mut manifest_output) = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(manifest_path)
        else {
            tcx.dcx()
                .fatal("Supercov could not create a unique Rust manifest candidate");
        };
        if manifest_output.write_all(manifest.as_bytes()).is_err()
            || manifest_output.flush().is_err()
        {
            tcx.dcx()
                .fatal("Supercov could not persist the Rust manifest candidate");
        }
        Compilation::Continue
    }
}

fn install_query_overrides(_session: &Session, providers: &mut Providers) {
    let _ = ORIGINAL_OPTIMIZED_MIR.set(providers.queries.optimized_mir);
    let _ = ORIGINAL_MIR_FOR_CTFE.set(providers.queries.mir_for_ctfe);
    let _ = ORIGINAL_MIR_BUILT.set(providers.queries.mir_built);
    let _ = ORIGINAL_MIR_DROPS.set(providers.queries.mir_drops_elaborated_and_const_checked);
    providers.queries.optimized_mir = optimized_mir_with_probe;
    providers.queries.mir_for_ctfe = mir_for_ctfe_with_markers;
    providers.queries.mir_built = mir_built_with_match_markers;
    providers.queries.mir_drops_elaborated_and_const_checked = mir_drops_with_structural_probes;
}

fn semantic_successors(terminator: &Terminator<'_>) -> Vec<BasicBlock> {
    match terminator.kind {
        TerminatorKind::FalseEdge { real_target, .. }
        | TerminatorKind::FalseUnwind { real_target, .. } => vec![real_target],
        _ => terminator.successors().collect(),
    }
}

fn block_reaches(body: &Body<'_>, start: BasicBlock, target: BasicBlock) -> bool {
    let mut pending = vec![start];
    let mut visited = BTreeSet::new();
    while let Some(block) = pending.pop() {
        if block == target {
            return true;
        }
        if !visited.insert(block) {
            continue;
        }
        pending.extend(semantic_successors(body.basic_blocks[block].terminator()));
    }
    false
}

fn guarded_match_arm_entry(
    body: &Body<'_>,
    candidate: BasicBlock,
    rejection: BasicBlock,
) -> Result<BasicBlock, String> {
    let mut pending = vec![candidate];
    let mut visited = BTreeSet::new();
    let mut selected = BTreeSet::new();
    while let Some(block) = pending.pop() {
        if block == rejection || !visited.insert(block) {
            continue;
        }
        let terminator = body.basic_blocks[block].terminator();
        let successors = terminator.successors().collect::<BTreeSet<_>>();
        if matches!(terminator.kind, TerminatorKind::SwitchInt { .. }) {
            let (rejecting, accepting): (Vec<_>, Vec<_>) = successors
                .iter()
                .copied()
                .partition(|successor| block_reaches(body, *successor, rejection));
            if !rejecting.is_empty() && accepting.len() == 1 {
                selected.insert(accepting[0]);
            }
        }
        pending.extend(semantic_successors(terminator));
    }
    let selected = selected.into_iter().collect::<Vec<_>>();
    let [selected] = selected.as_slice() else {
        return Err("guarded collapsed match has no unique accepting edge".into());
    };
    Ok(*selected)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SyntheticMatchArmPath {
    entry: BasicBlock,
    guard_candidate: Option<BasicBlock>,
    rejection: Option<BasicBlock>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct SyntheticMatchGroupPath {
    start: BasicBlock,
    arms: Vec<SyntheticMatchArmPath>,
}

fn guard_condition_blocks<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    candidate: BasicBlock,
    selected: BasicBlock,
    rejection: BasicBlock,
    expected: usize,
) -> Result<Vec<BasicBlock>, String> {
    let mut pending = vec![candidate];
    let mut visited = BTreeSet::new();
    let mut switches = BTreeSet::new();
    while let Some(block) = pending.pop() {
        if block == selected || block == rejection || !visited.insert(block) {
            continue;
        }
        let terminator = body.basic_blocks[block].terminator();
        if let TerminatorKind::SwitchInt { discr, targets } = &terminator.kind
            && discr.ty(&body.local_decls, tcx) == tcx.types.bool
        {
            let true_target = targets.target_for_value(1);
            let false_target = targets.target_for_value(0);
            let terminal_reachability = |target| {
                (
                    block_reaches(body, target, selected),
                    block_reaches(body, target, rejection),
                )
            };
            if terminal_reachability(true_target) != terminal_reachability(false_target) {
                switches.insert(block);
            }
        }
        pending.extend(semantic_successors(terminator));
    }
    if switches.len() != expected {
        return Err(format!(
            "synthetic guard has {} Boolean switches for {expected} conditions",
            switches.len()
        ));
    }
    let mut ranked = switches
        .iter()
        .copied()
        .map(|block| {
            let predecessors = switches
                .iter()
                .copied()
                .filter(|other| *other != block && block_reaches(body, *other, block))
                .count();
            (predecessors, block)
        })
        .collect::<Vec<_>>();
    ranked.sort();
    if ranked
        .iter()
        .enumerate()
        .any(|(index, (rank, _))| index != *rank)
    {
        return Err("synthetic guard Boolean switches have no total evaluation order".into());
    }
    Ok(ranked.into_iter().map(|(_, block)| block).collect())
}

fn synthetic_match_candidates(
    tcx: TyCtxt<'_>,
    crate_name: &str,
    body: &Body<'_>,
    group: &MatchSelectionObligation,
) -> Vec<SyntheticMatchGroupPath> {
    let arm_count = group.arms.len();
    if arm_count < 2 {
        return Vec::new();
    }
    let false_edges = body
        .basic_blocks
        .iter_enumerated()
        .filter_map(|(block, data)| match data.terminator().kind {
            TerminatorKind::FalseEdge {
                real_target,
                imaginary_target,
            } => Some((block, real_target, imaginary_target)),
            _ => None,
        })
        .collect::<Vec<_>>();
    let by_block = false_edges
        .into_iter()
        .map(|(block, real, imaginary)| (block, (real, imaginary)))
        .collect::<BTreeMap<_, _>>();
    let mut candidates = Vec::new();
    for head in by_block.keys().copied() {
        let head_source = body.basic_blocks[head]
            .terminator()
            .source_info
            .span
            .source_callsite();
        if !stable_source_range(tcx, head_source, crate_name)
            .is_ok_and(|source| source == group.identity.source)
        {
            continue;
        }
        let mut current = head;
        let mut entries = Vec::with_capacity(arm_count);
        let mut valid = true;
        for (index, arm) in group.arms[..arm_count - 1].iter().enumerate() {
            let Some((real, imaginary)) = by_block.get(&current).copied() else {
                valid = false;
                break;
            };
            let entry = if arm.guarded {
                match guarded_match_arm_entry(body, real, imaginary) {
                    Ok(entry) => entry,
                    Err(_) => {
                        valid = false;
                        break;
                    }
                }
            } else {
                real
            };
            entries.push(SyntheticMatchArmPath {
                entry,
                guard_candidate: arm.guarded.then_some(real),
                rejection: arm.guarded.then_some(imaginary),
            });
            if index + 1 == arm_count - 1 {
                entries.push(SyntheticMatchArmPath {
                    entry: imaginary,
                    guard_candidate: None,
                    rejection: None,
                });
            } else {
                current = imaginary;
            }
        }
        if valid
            && entries.len() == arm_count
            && entries
                .iter()
                .map(|entry| entry.entry)
                .collect::<BTreeSet<_>>()
                .len()
                == entries.len()
        {
            candidates.push(SyntheticMatchGroupPath {
                start: head,
                arms: entries,
            });
        }
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn synthetic_match_parent_relation(
    body: &Body<'_>,
    child_group: &MatchSelectionObligation,
    child: &SyntheticMatchGroupPath,
    parent: &SyntheticMatchGroupPath,
) -> bool {
    match (child_group.parent_site, child_group.parent_arm_index) {
        (Some("scrutinee"), None) => {
            block_reaches(body, child.start, parent.start)
                && !block_reaches(body, parent.start, child.start)
        }
        (Some("body"), Some(index)) => parent.arms.get(index).is_some_and(|arm| {
            block_reaches(body, arm.entry, child.start)
                && parent.arms.iter().enumerate().all(|(other, arm)| {
                    other == index || !block_reaches(body, arm.entry, child.start)
                })
        }),
        (Some("guard"), Some(index)) => parent.arms.get(index).is_some_and(|arm| {
            arm.guard_candidate.is_some_and(|candidate| {
                block_reaches(body, candidate, child.start)
                    && block_reaches(body, child.start, arm.entry)
                    && arm
                        .rejection
                        .is_some_and(|rejection| block_reaches(body, child.start, rejection))
            })
        }),
        _ => false,
    }
}

fn synthetic_match_assignments(
    tcx: TyCtxt<'_>,
    crate_name: &str,
    body: &Body<'_>,
    groups: &[&MatchSelectionObligation],
) -> Result<BTreeMap<String, SyntheticMatchGroupPath>, String> {
    let candidates = groups
        .iter()
        .map(|group| {
            (
                group.identity.id.clone(),
                synthetic_match_candidates(tcx, crate_name, body, group),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if let Some((group_id, paths)) = candidates.iter().find(|(_, paths)| paths.is_empty()) {
        return Err(format!(
            "collapsed match group {group_id} has {} structurally valid arm chains",
            paths.len()
        ));
    }
    fn recurse(
        body: &Body<'_>,
        groups: &[&MatchSelectionObligation],
        candidates: &BTreeMap<String, Vec<SyntheticMatchGroupPath>>,
        index: usize,
        used_starts: &mut BTreeSet<u32>,
        current: &mut BTreeMap<String, SyntheticMatchGroupPath>,
        solutions: &mut Vec<BTreeMap<String, SyntheticMatchGroupPath>>,
    ) {
        if solutions.len() > 1 {
            return;
        }
        if index == groups.len() {
            let valid = groups.iter().all(|group| {
                let Some(parent_id) = &group.parent_group_id else {
                    return true;
                };
                let (Some(child), Some(parent)) =
                    (current.get(&group.identity.id), current.get(parent_id))
                else {
                    return false;
                };
                synthetic_match_parent_relation(body, group, child, parent)
            });
            if valid {
                solutions.push(current.clone());
            }
            return;
        }
        let group = groups[index];
        for candidate in &candidates[&group.identity.id] {
            if !used_starts.insert(candidate.start.as_u32()) {
                continue;
            }
            current.insert(group.identity.id.clone(), candidate.clone());
            recurse(
                body,
                groups,
                candidates,
                index + 1,
                used_starts,
                current,
                solutions,
            );
            current.remove(&group.identity.id);
            used_starts.remove(&candidate.start.as_u32());
        }
    }
    let mut ordered = groups.to_vec();
    ordered.sort_by_key(|group| candidates[&group.identity.id].len());
    let mut solutions = Vec::new();
    recurse(
        body,
        &ordered,
        &candidates,
        0,
        &mut BTreeSet::new(),
        &mut BTreeMap::new(),
        &mut solutions,
    );
    let [solution] = solutions.as_slice() else {
        return Err(format!(
            "{} collapsed match groups have {} parent-consistent CFG assignments; candidates={candidates:?}; solutions={solutions:?}",
            groups.len(),
            solutions.len()
        ));
    };
    Ok(solution.clone())
}

fn match_arm_marker_statement<'tcx>(
    tcx: TyCtxt<'tcx>,
    marker_local: rustc_middle::mir::Local,
    ordinal: u64,
) -> Statement<'tcx> {
    Statement::new(
        SourceInfo::outermost(DUMMY_SP),
        StatementKind::Assign(Box::new((
            Place::from(marker_local),
            Rvalue::Use(Operand::const_from_scalar(
                tcx,
                tcx.types.u64,
                Scalar::from_u64(ordinal),
                DUMMY_SP,
            )),
        ))),
    )
}

fn mir_built_with_match_markers<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx Steal<Body<'tcx>> {
    let original = ORIGINAL_MIR_BUILT
        .get()
        .expect("original mir_built provider");
    let body = original(tcx, def_id);
    if env::var_os(INSTRUMENT_MIR).is_none() || tcx.hir_body_const_context(def_id).is_some() {
        return body;
    }
    let Some(obligations) = runtime_body_obligations(tcx, def_id) else {
        return body;
    };
    {
        let borrowed = body.borrow();
        let block_sources = executable_block_sources(tcx, &obligations.crate_name, &borrowed);
        let mut reachable = BTreeSet::new();
        let mut pending = vec![rustc_middle::mir::START_BLOCK];
        while let Some(block) = pending.pop() {
            if reachable.insert(block) {
                pending.extend(semantic_successors(
                    borrowed.basic_blocks[block].terminator(),
                ));
            }
        }
        let unreachable = obligations
            .match_groups
            .values()
            .filter(|group| {
                group.definitions.contains(&obligations.definition)
                    && group.identity.provenance != "synthetic-expansion"
            })
            .flat_map(|group| &group.arms)
            .filter(|arm| {
                !block_sources.iter().any(|(block, sources)| {
                    reachable.contains(block)
                        && sources.iter().any(|source| {
                            source.key == arm.body_source.key
                                && source.start >= arm.body_source.start
                                && source.end <= arm.body_source.end
                        })
                })
            })
            .map(|arm| arm.branch_id.clone())
            .collect::<Vec<_>>();
        UNREACHABLE_MATCH_ARMS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .extend(unreachable);
    }
    let synthetic_groups = obligations
        .match_groups
        .values()
        .filter(|group| {
            group.definitions.contains(&obligations.definition)
                && group.identity.provenance == "synthetic-expansion"
        })
        .collect::<Vec<_>>();
    if synthetic_groups.is_empty() {
        return body;
    }
    let (assignments, guard_blocks) = {
        let borrowed = body.borrow();
        let assignments = match synthetic_match_assignments(
            tcx,
            &obligations.crate_name,
            &borrowed,
            &synthetic_groups,
        ) {
            Ok(assignments) => assignments,
            Err(error) => tcx.dcx().fatal(format!(
                "Supercov could not bind pre-borrow-check synthetic matches in {}: {error}",
                obligations.definition
            )),
        };
        let mut guard_blocks = Vec::new();
        for group in &synthetic_groups {
            let Some(path) = assignments.get(&group.identity.id) else {
                return body;
            };
            for (arm, arm_path) in group.arms.iter().zip(&path.arms) {
                let Some(decision_id) = &arm.guard_decision_id else {
                    continue;
                };
                let Some(decision) = obligations.decisions.get(decision_id) else {
                    return body;
                };
                let (Some(candidate), Some(rejection)) =
                    (arm_path.guard_candidate, arm_path.rejection)
                else {
                    return body;
                };
                let blocks = match guard_condition_blocks(
                    tcx,
                    &borrowed,
                    candidate,
                    arm_path.entry,
                    rejection,
                    decision.conditions.len(),
                ) {
                    Ok(blocks) => blocks,
                    Err(error) => tcx.dcx().fatal(format!(
                        "Supercov could not bind pre-borrow-check synthetic guard {} in {}: {error}",
                        decision_id, obligations.definition
                    )),
                };
                guard_blocks.extend(
                    blocks
                        .into_iter()
                        .enumerate()
                        .map(|(index, block)| (decision_id.clone(), index, block)),
                );
            }
        }
        (assignments, guard_blocks)
    };
    let mut instrumented = body.steal();
    let mut local_ordinals = BTreeMap::new();
    for group in &synthetic_groups {
        let path = &assignments[&group.identity.id];
        for (arm, arm_path) in group.arms.iter().zip(&path.arms) {
            let marker_local = instrumented
                .local_decls
                .push(LocalDecl::new(tcx.types.u64, DUMMY_SP));
            instrumented.basic_blocks_mut()[arm_path.entry]
                .statements
                .insert(
                    0,
                    match_arm_marker_statement(tcx, marker_local, arm.selected_ordinal),
                );
            local_ordinals.insert(marker_local.as_u32(), arm.selected_ordinal);
        }
    }
    let mut guard_markers = Vec::new();
    for (decision_id, condition_index, block) in guard_blocks {
        let marker_local = instrumented
            .local_decls
            .push(LocalDecl::new(tcx.types.u64, DUMMY_SP));
        instrumented.basic_blocks_mut()[block].statements.insert(
            0,
            match_arm_marker_statement(tcx, marker_local, condition_index as u64),
        );
        guard_markers.push(MatchGuardConditionMarker {
            local: marker_local.as_u32(),
            decision_id,
            condition_index,
        });
    }
    let mut markers = MATCH_ARM_MARKERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(existing) = markers.insert(obligations.definition.clone(), local_ordinals.clone())
        && existing != local_ordinals
    {
        tcx.dcx().fatal(format!(
            "Supercov synthetic match marker collision for {}",
            obligations.definition
        ));
    }
    if !guard_markers.is_empty() {
        let mut markers = MATCH_GUARD_MARKERS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) =
            markers.insert(obligations.definition.clone(), guard_markers.clone())
            && existing != guard_markers
        {
            tcx.dcx().fatal(format!(
                "Supercov synthetic match guard marker collision for {}",
                obligations.definition
            ));
        }
    }
    tcx.alloc_steal_mir(instrumented)
}

fn mir_drops_with_structural_probes<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
) -> &'tcx Steal<Body<'tcx>> {
    let original = ORIGINAL_MIR_DROPS
        .get()
        .expect("original mir_drops_elaborated_and_const_checked provider");
    let body = original(tcx, def_id);
    if env::var_os(INSTRUMENT_MIR).is_none() || tcx.hir_body_const_context(def_id).is_some() {
        return body;
    }
    let (match_plans, for_plans, guard_plans) = {
        let borrowed = body.borrow();
        (
            runtime_match_plans(tcx, def_id, &borrowed),
            runtime_for_loop_plans(tcx, def_id, &borrowed),
            runtime_marked_guard_decision_plans(tcx, def_id, &borrowed),
        )
    };
    let match_plans = match_plans.unwrap_or_else(|error| {
        tcx.dcx().fatal(format!(
            "Supercov could not bind pre-optimization Rust match probes in {}: {error}",
            tcx.def_path_str(def_id)
        ))
    });
    let for_plans = for_plans.unwrap_or_else(|error| {
        tcx.dcx().fatal(format!(
            "Supercov could not bind pre-optimization Rust for-loop probes in {}: {error}",
            tcx.def_path_str(def_id)
        ))
    });
    let guard_plans = guard_plans.unwrap_or_else(|error| {
        tcx.dcx().fatal(format!(
            "Supercov could not bind pre-optimization Rust synthetic guard probes in {}: {error}",
            tcx.def_path_str(def_id)
        ))
    });
    if match_plans.is_empty() && for_plans.is_empty() && guard_plans.is_empty() {
        return body;
    }
    let has_branch_plans = !match_plans.is_empty() || !for_plans.is_empty();
    let start_branch = has_branch_plans
        .then(|| find_runtime_function(tcx, START_BRANCH_FUNCTION))
        .flatten();
    let hit_branch = has_branch_plans
        .then(|| find_runtime_function(tcx, HIT_BRANCH_FUNCTION))
        .flatten();
    let has_guard_plans = !guard_plans.is_empty();
    let start_decision = has_guard_plans
        .then(|| find_runtime_function(tcx, START_DECISION_FUNCTION))
        .flatten();
    let record_condition = has_guard_plans
        .then(|| find_runtime_function(tcx, RECORD_CONDITION_FUNCTION))
        .flatten();
    let finish_decision = has_guard_plans
        .then(|| find_runtime_function(tcx, FINISH_DECISION_FUNCTION))
        .flatten();
    if has_branch_plans != (start_branch.is_some() && hit_branch.is_some())
        || has_guard_plans
            != (start_decision.is_some() && record_condition.is_some() && finish_decision.is_some())
    {
        tcx.dcx().fatal(format!(
            "Supercov structural runtimes are incomplete while instrumenting {}",
            tcx.def_path_str(def_id)
        ));
    }
    let mut instrumented = body.steal();
    let span = tcx.def_span(def_id);
    let unit = instrumented
        .local_decls
        .push(LocalDecl::new(tcx.types.unit, span));
    if let Err(error) = strip_match_arm_markers(&mut instrumented, &tcx.def_path_str(def_id)) {
        tcx.dcx().fatal(format!(
            "Supercov could not consume pre-borrow-check Rust match markers in {}: {error}",
            tcx.def_path_str(def_id)
        ));
    }
    let mut match_plans = match_plans;
    if let (Some(start), Some(hit)) = (start_branch, hit_branch)
        && let Err(error) = instrument_runtime_matches(
            tcx,
            &mut instrumented,
            &mut match_plans,
            start,
            hit,
            unit,
            span,
        )
    {
        tcx.dcx().fatal(format!(
            "Supercov could not inject pre-optimization Rust match probes in {}: {error}",
            tcx.def_path_str(def_id)
        ));
    }
    // Match instrumentation may replace the accepting edge of a guard. Bind
    // exact Boolean targets after that edit while the semantic markers remain.
    let guard_plans =
        runtime_marked_guard_decision_plans(tcx, def_id, &instrumented).unwrap_or_else(|error| {
            tcx.dcx().fatal(format!(
                "Supercov could not rebind pre-optimization Rust synthetic guard probes in {}: {error}",
                tcx.def_path_str(def_id)
            ))
        });
    if let Err(error) = strip_match_guard_markers(&mut instrumented, &tcx.def_path_str(def_id)) {
        tcx.dcx().fatal(format!(
            "Supercov could not consume pre-borrow-check Rust match guard markers in {}: {error}",
            tcx.def_path_str(def_id)
        ));
    }
    if let (Some(start), Some(condition), Some(finish)) =
        (start_decision, record_condition, finish_decision)
        && let Err(error) = instrument_runtime_decisions(
            tcx,
            &mut instrumented,
            &guard_plans,
            DecisionRuntime {
                start,
                condition,
                finish,
                branch_hit: None,
                unit,
            },
            span,
        )
    {
        tcx.dcx().fatal(format!(
            "Supercov could not inject pre-optimization Rust synthetic guard probes in {}: {error}",
            tcx.def_path_str(def_id)
        ));
    }
    // Match instrumentation can split blocks enclosing a nested for loop, so
    // bind for-loop structure again against the current body before editing it.
    let mut for_plans =
        runtime_for_loop_plans(tcx, def_id, &instrumented).unwrap_or_else(|error| {
            tcx.dcx().fatal(format!(
                "Supercov could not rebind pre-optimization Rust for-loop probes in {}: {error}",
                tcx.def_path_str(def_id)
            ))
        });
    if let (Some(start), Some(hit)) = (start_branch, hit_branch)
        && let Err(error) = instrument_runtime_for_loops(
            tcx,
            &mut instrumented,
            &mut for_plans,
            start,
            hit,
            unit,
            span,
        )
    {
        tcx.dcx().fatal(format!(
            "Supercov could not inject pre-optimization Rust for-loop probes in {}: {error}",
            tcx.def_path_str(def_id)
        ));
    }
    tcx.alloc_steal_mir(instrumented)
}

fn mir_for_ctfe_with_markers<'tcx>(tcx: TyCtxt<'tcx>, def_id: LocalDefId) -> &'tcx Body<'tcx> {
    let original = ORIGINAL_MIR_FOR_CTFE
        .get()
        .expect("original mir_for_ctfe provider");
    let body = original(tcx, def_id);
    if env::var_os(INSTRUMENT_CTFE).is_none()
        || !tcx.def_path_str(def_id).ends_with("const_decision")
    {
        return body;
    }

    let mut instrumented = body.clone();
    let span = tcx.def_span(def_id);
    let marker_local = instrumented
        .local_decls
        .push(LocalDecl::new(tcx.types.u64, span));
    for (block, block_data) in instrumented.basic_blocks_mut().iter_enumerated_mut() {
        let marker = CTFE_MARKER_PREFIX | u64::from(block.as_u32());
        block_data
            .statements
            .insert(0, ctfe_marker_statement(tcx, marker_local, marker, span));
    }

    let decision_edges = instrumented
        .basic_blocks
        .iter_enumerated()
        .filter_map(|(source, block)| {
            let targets = block.terminator().successors().collect::<Vec<_>>();
            (targets.len() > 1).then_some(
                targets
                    .into_iter()
                    .enumerate()
                    .map(move |(edge, target)| (source, edge, target)),
            )
        })
        .flatten()
        .collect::<Vec<_>>();
    let mut bridges = Vec::with_capacity(decision_edges.len());
    for (ordinal, (source, edge, target)) in decision_edges.iter().copied().enumerate() {
        let marker = CTFE_MARKER_PREFIX | CTFE_EDGE_MARKER_OFFSET | ordinal as u64;
        let mut bridge = BasicBlockData::new(
            Some(Terminator {
                source_info: SourceInfo::outermost(span),
                kind: TerminatorKind::Goto { target },
            }),
            false,
        );
        bridge
            .statements
            .push(ctfe_marker_statement(tcx, marker_local, marker, span));
        let bridge = instrumented.basic_blocks_mut().push(bridge);
        bridges.push((source, edge, bridge));
    }
    for (source, edge, bridge) in bridges {
        let mut current = 0;
        instrumented.basic_blocks_mut()[source]
            .terminator_mut()
            .successors_mut(|target| {
                if current == edge {
                    *target = bridge;
                }
                current += 1;
            });
    }
    tcx.arena.alloc(instrumented)
}

fn ctfe_marker_statement<'tcx>(
    tcx: TyCtxt<'tcx>,
    marker_local: rustc_middle::mir::Local,
    marker: u64,
    span: rustc_span::Span,
) -> Statement<'tcx> {
    Statement::new(
        SourceInfo::outermost(span),
        StatementKind::Assign(Box::new((
            Place::from(marker_local),
            Rvalue::Use(Operand::const_from_scalar(
                tcx,
                tcx.types.u64,
                Scalar::from_u64(marker),
                span,
            )),
        ))),
    )
}

#[derive(Debug)]
struct RuntimeDecisionCondition {
    index: u64,
    entry_block: BasicBlock,
    true_sources: Vec<BasicBlock>,
    false_sources: Vec<BasicBlock>,
    true_target: BasicBlock,
    false_target: BasicBlock,
    true_outcome: Option<bool>,
    false_outcome: Option<bool>,
}

fn nearest_common_dominator(
    dominators: &rustc_data_structures::graph::dominators::Dominators<BasicBlock>,
    first: BasicBlock,
    second: BasicBlock,
) -> Option<BasicBlock> {
    let mut candidate = second;
    loop {
        if dominators.dominates(candidate, first) {
            return Some(candidate);
        }
        candidate = dominators.immediate_dominator(candidate)?;
    }
}

#[derive(Debug)]
struct RuntimeDecisionPlan {
    id: String,
    id_high: u64,
    id_low: u32,
    conditions: Vec<RuntimeDecisionCondition>,
    loop_alternatives: Option<(u64, u64)>,
    loop_source: Option<StableSourceRange>,
    loop_token: Option<rustc_middle::mir::Local>,
}

struct RuntimeBodyObligations {
    definition: String,
    crate_name: String,
    branches: BTreeMap<String, BranchObligation>,
    decisions: BTreeMap<String, DecisionObligation>,
    match_groups: BTreeMap<String, MatchSelectionObligation>,
}

fn runtime_body_obligations(tcx: TyCtxt<'_>, def_id: LocalDefId) -> Option<RuntimeBodyObligations> {
    let definition = tcx.def_path_str(def_id);
    if definition.contains("__supercov_spike_runtime") {
        return None;
    }
    let hir_body = tcx.hir_maybe_body_owned_by(def_id)?;
    let crate_name = tcx.crate_name(rustc_span::def_id::LOCAL_CRATE).to_string();
    let mut points = BTreeMap::new();
    let mut branches = BTreeMap::new();
    let mut decisions = BTreeMap::new();
    let mut match_groups = BTreeMap::new();
    let mut limitations = BTreeSet::new();
    HirManifestCollector {
        tcx,
        def_id: def_id.to_def_id(),
        crate_name: &crate_name,
        definition: definition.clone(),
        // Function identity occupies the stable owner-local ordinal zero.
        ordinal: 1,
        points: &mut points,
        branches: &mut branches,
        decisions: &mut decisions,
        match_groups: &mut match_groups,
        limitations: &mut limitations,
        control_overrides: BTreeMap::new(),
        match_context: None,
    }
    .visit_body(hir_body);
    prune_unreachable_match_arms(&mut branches, &mut match_groups);
    Some(RuntimeBodyObligations {
        definition,
        crate_name,
        branches,
        decisions,
        match_groups,
    })
}

fn runtime_decision_plans<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    body: &Body<'tcx>,
) -> Result<Vec<RuntimeDecisionPlan>, String> {
    let Some(obligations) = runtime_body_obligations(tcx, def_id) else {
        return Ok(Vec::new());
    };
    let RuntimeBodyObligations {
        definition,
        crate_name,
        branches,
        decisions,
        match_groups: _,
    } = obligations;
    if decisions.is_empty() {
        return Ok(Vec::new());
    }
    let coverage = body.function_coverage_info.as_deref();
    if coverage.is_none() && !tcx.def_span(def_id).from_expansion() {
        return Err(format!(
            "rustc did not retain branch mappings for decision-bearing function {definition}"
        ));
    }
    let mut bcb_blocks = BTreeMap::<u32, Vec<BasicBlock>>::new();
    for (block, data) in body.basic_blocks.iter_enumerated() {
        for statement in &data.statements {
            if let StatementKind::Coverage(CoverageKind::VirtualCounter { bcb }) = statement.kind {
                bcb_blocks.entry(bcb.as_u32()).or_default().push(block);
            }
        }
    }
    let mut branch_mappings = coverage
        .into_iter()
        .flat_map(|coverage| coverage.mappings.iter())
        .filter_map(|mapping| match mapping.kind {
            MappingKind::Branch {
                true_bcb,
                false_bcb,
            } => Some((mapping.span, true_bcb.as_u32(), false_bcb.as_u32())),
            MappingKind::Code { .. } => None,
        })
        .map(|(span, true_bcb, false_bcb)| {
            stable_source_range(tcx, span, &crate_name).map(|source| (source, true_bcb, false_bcb))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let dominators = body.basic_blocks.dominators().clone();
    let mut plans = Vec::new();
    let mut fallback_blocks = BTreeSet::new();
    for decision in decisions.values().filter(|decision| {
        !(decision.decision_kind == "match-guard"
            && decision.identity.provenance == "synthetic-expansion")
    }) {
        let digest = decision
            .identity
            .id
            .strip_prefix("rs:decision:")
            .ok_or_else(|| format!("invalid Rust decision ID {}", decision.identity.id))?;
        if digest.len() != 24 {
            return Err(format!("invalid Rust decision digest {digest}"));
        }
        let id_high = u64::from_str_radix(&digest[..16], 16).map_err(|error| {
            format!("invalid Rust decision ID {}: {error}", decision.identity.id)
        })?;
        let id_low = u32::from_str_radix(&digest[16..], 16).map_err(|error| {
            format!("invalid Rust decision ID {}: {error}", decision.identity.id)
        })?;
        let mut conditions = Vec::new();
        for (index, condition) in decision.conditions.iter().enumerate() {
            let mapping_index = branch_mappings
                .iter()
                .position(|(source, _, _)| source == &condition.branch_source);
            let (entry_block, true_sources, false_sources, true_target, false_target) =
                if let Some(mapping_index) = mapping_index {
                    let (_, true_bcb, false_bcb) = branch_mappings.remove(mapping_index);
                    let unique_block = |bcb: u32| -> Result<BasicBlock, String> {
                        let blocks = bcb_blocks.get(&bcb).cloned().unwrap_or_default();
                        match blocks.as_slice() {
                            [block] => Ok(*block),
                            _ => Err(format!(
                                "coverage block {bcb} for {} maps to {} MIR blocks",
                                decision.identity.id,
                                blocks.len()
                            )),
                        }
                    };
                    let true_target = unique_block(true_bcb)?;
                    let false_target = unique_block(false_bcb)?;
                    if true_target == false_target {
                        return Err(format!(
                            "condition {} of {} has one true/false target",
                            index, decision.identity.id
                        ));
                    }
                    let entry_block =
                        nearest_common_dominator(&dominators, true_target, false_target)
                            .ok_or_else(|| {
                                format!(
                                    "condition {} of {} has no common MIR dominator",
                                    index, decision.identity.id
                                )
                            })?;
                    let incoming = |target: BasicBlock| {
                        body.basic_blocks.predecessors()[target]
                            .iter()
                            .copied()
                            .filter(|source| dominators.dominates(entry_block, *source))
                            .collect::<Vec<_>>()
                    };
                    let true_sources = incoming(true_target);
                    let false_sources = incoming(false_target);
                    if true_sources.is_empty() || false_sources.is_empty() {
                        return Err(format!(
                            "condition {} of {} has incomplete terminal edges ({}/{})",
                            index,
                            decision.identity.id,
                            true_sources.len(),
                            false_sources.len()
                        ));
                    }
                    (
                        entry_block,
                        true_sources,
                        false_sources,
                        true_target,
                        false_target,
                    )
                } else if tcx.def_span(def_id).from_expansion() {
                    let source_blocks = body
                        .basic_blocks
                        .iter_enumerated()
                        .filter_map(|(block, data)| {
                            if fallback_blocks.contains(&block.as_u32()) {
                                return None;
                            }
                            let TerminatorKind::SwitchInt { discr, targets } =
                                &data.terminator().kind
                            else {
                                return None;
                            };
                            if discr.ty(&body.local_decls, tcx) != tcx.types.bool {
                                return None;
                            }
                            let source = stable_source_range(
                                tcx,
                                data.terminator().source_info.span,
                                &crate_name,
                            )
                            .ok()?;
                            (source == condition.branch_source || source == condition.source)
                                .then_some((
                                    block,
                                    targets.target_for_value(1),
                                    targets.target_for_value(0),
                                ))
                        })
                        .collect::<Vec<_>>();
                    let [(source_block, true_target, false_target)] = source_blocks.as_slice()
                    else {
                        return Err(format!(
                            "could not bind one expanded boolean MIR branch for {} condition {}; found {}",
                            decision.identity.id,
                            index,
                            source_blocks.len()
                        ));
                    };
                    fallback_blocks.insert(source_block.as_u32());
                    (
                        *source_block,
                        vec![*source_block],
                        vec![*source_block],
                        *true_target,
                        *false_target,
                    )
                } else {
                    return Err(format!(
                        "rustc branch mapping missing for {} condition {} at {}:{}-{}; available: {:?}",
                        decision.identity.id,
                        index,
                        condition.branch_source.key,
                        condition.branch_source.start,
                        condition.branch_source.end,
                        branch_mappings
                            .iter()
                            .map(|(source, _, _)| format!(
                                "{}:{}-{}:{}",
                                source.key, source.start, source.end, source.class
                            ))
                            .collect::<Vec<_>>(),
                    ));
                };
            conditions.push(RuntimeDecisionCondition {
                index: index as u64,
                entry_block,
                true_sources,
                false_sources,
                true_target,
                false_target,
                true_outcome: condition.true_outcome,
                false_outcome: condition.false_outcome,
            });
        }
        let (loop_alternatives, loop_source) = if decision.decision_kind.starts_with("while") {
            let candidates = branches
                .values()
                .filter(|branch| {
                    branch.branch_kind == "loop-entry"
                        && branch.discriminator == "loop-entry:while"
                        && branch.identity.source.key == decision.identity.source.key
                        && branch.identity.source.start <= decision.identity.source.start
                        && branch.identity.source.end >= decision.identity.source.end
                        && branch.definitions.contains(&definition)
                })
                .collect::<Vec<_>>();
            let [branch] = candidates.as_slice() else {
                return Err(format!(
                    "while decision {} maps to {} invocation branches",
                    decision.identity.id,
                    candidates.len()
                ));
            };
            let zero = branch
                .alternatives
                .iter()
                .find(|alternative| alternative.label == "zero iterations")
                .map(|alternative| alternative.identity.probe_ordinal);
            let entered = branch
                .alternatives
                .iter()
                .find(|alternative| alternative.label == "entered")
                .map(|alternative| alternative.identity.probe_ordinal);
            match (zero, entered) {
                (Some(zero), Some(entered)) => {
                    (Some((zero, entered)), Some(branch.identity.source.clone()))
                }
                _ => {
                    return Err(format!(
                        "while invocation branch {} has incomplete alternatives",
                        branch.identity.id
                    ));
                }
            }
        } else {
            (None, None)
        };
        plans.push(RuntimeDecisionPlan {
            id: decision.identity.id.clone(),
            id_high,
            id_low,
            conditions,
            loop_alternatives,
            loop_source,
            loop_token: None,
        });
    }
    Ok(plans)
}

#[derive(Debug)]
struct RuntimeForLoopPlan {
    id: String,
    switch_block: BasicBlock,
    zero_target: BasicBlock,
    entered_target: BasicBlock,
    token: Option<rustc_middle::mir::Local>,
    zero_ordinal: u64,
    entered_ordinal: u64,
}

fn option_switch_targets<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &Body<'tcx>,
    block: BasicBlock,
) -> Option<(BasicBlock, BasicBlock)> {
    let TerminatorKind::SwitchInt { discr, targets } = &body.basic_blocks[block].terminator().kind
    else {
        return None;
    };
    let discriminant_local = discr.place()?.as_local()?;
    let enum_place = body.basic_blocks[block]
        .statements
        .iter()
        .rev()
        .find_map(|statement| {
            let StatementKind::Assign(assignment) = &statement.kind else {
                return None;
            };
            let (destination, value) = &**assignment;
            if destination.as_local() != Some(discriminant_local) {
                return None;
            }
            let Rvalue::Discriminant(place) = value else {
                return None;
            };
            Some(*place)
        })?;
    let ty::Adt(definition, _) = enum_place.ty(&body.local_decls, tcx).ty.kind() else {
        return None;
    };
    let none = tcx.lang_items().option_none_variant()?;
    let some = tcx.lang_items().option_some_variant()?;
    if tcx.parent(none) != definition.did() || tcx.parent(some) != definition.did() {
        return None;
    }
    let none = definition
        .discriminant_for_variant(tcx, definition.variant_index_with_id(none))
        .val;
    let some = definition
        .discriminant_for_variant(tcx, definition.variant_index_with_id(some))
        .val;
    Some((
        targets.target_for_value(none),
        targets.target_for_value(some),
    ))
}

fn runtime_for_loop_plans<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    body: &Body<'tcx>,
) -> Result<Vec<RuntimeForLoopPlan>, String> {
    let Some(obligations) = runtime_body_obligations(tcx, def_id) else {
        return Ok(Vec::new());
    };
    let for_branches = obligations
        .branches
        .values()
        .filter(|branch| {
            branch.branch_kind == "loop-entry"
                && branch.discriminator == "loop-entry:for"
                && branch.definitions.contains(&obligations.definition)
        })
        .collect::<Vec<_>>();
    let mut plans = Vec::new();
    for branch in &for_branches {
        let zero_ordinal = branch
            .alternatives
            .iter()
            .find(|alternative| alternative.label == "zero iterations")
            .map(|alternative| alternative.identity.probe_ordinal)
            .ok_or_else(|| format!("for branch {} has no zero alternative", branch.identity.id))?;
        let entered_ordinal = branch
            .alternatives
            .iter()
            .find(|alternative| alternative.label == "entered")
            .map(|alternative| alternative.identity.probe_ordinal)
            .ok_or_else(|| {
                format!(
                    "for branch {} has no entered alternative",
                    branch.identity.id
                )
            })?;
        let candidates = body
            .basic_blocks
            .iter_enumerated()
            .filter_map(|(block, data)| {
                let span = data.terminator().source_info.span;
                if span.desugaring_kind() != Some(DesugaringKind::ForLoop) {
                    return None;
                }
                let source =
                    stable_source_range(tcx, span.source_callsite(), &obligations.crate_name)
                        .ok()?;
                if source.key != branch.identity.source.key
                    || source.start < branch.identity.source.start
                    || source.end > branch.identity.source.end
                {
                    return None;
                }
                let owner = for_branches
                    .iter()
                    .filter(|candidate| {
                        candidate.identity.source.key == source.key
                            && candidate.identity.source.start <= source.start
                            && candidate.identity.source.end >= source.end
                    })
                    .min_by_key(|candidate| {
                        candidate
                            .identity
                            .source
                            .end
                            .saturating_sub(candidate.identity.source.start)
                    })?;
                if owner.identity.id != branch.identity.id {
                    return None;
                }
                option_switch_targets(tcx, body, block)
                    .map(|(zero_target, entered_target)| (block, zero_target, entered_target))
            })
            .collect::<Vec<_>>();
        let [(switch_block, zero_target, entered_target)] = candidates.as_slice() else {
            return Err(format!(
                "for branch {} maps to {} exact Option switches",
                branch.identity.id,
                candidates.len()
            ));
        };
        plans.push(RuntimeForLoopPlan {
            id: branch.identity.id.clone(),
            switch_block: *switch_block,
            zero_target: *zero_target,
            entered_target: *entered_target,
            token: None,
            zero_ordinal,
            entered_ordinal,
        });
    }
    Ok(plans)
}

#[derive(Debug)]
struct RuntimeMatchArm {
    branch_id: String,
    entry_block: BasicBlock,
    entry_sources: Vec<BasicBlock>,
    selected_ordinal: u64,
}

#[derive(Debug)]
struct RuntimeMatchPlan {
    id: String,
    start_block: BasicBlock,
    token: Option<rustc_middle::mir::Local>,
    arms: Vec<RuntimeMatchArm>,
}

fn executable_block_sources(
    tcx: TyCtxt<'_>,
    crate_name: &str,
    body: &Body<'_>,
) -> BTreeMap<BasicBlock, Vec<StableSourceRange>> {
    body.basic_blocks
        .iter_enumerated()
        .map(|(block, data)| {
            let mut sources = data
                .statements
                .iter()
                .filter(|statement| {
                    !matches!(
                        statement.kind,
                        StatementKind::StorageLive(_)
                            | StatementKind::StorageDead(_)
                            | StatementKind::PlaceMention(_)
                            | StatementKind::Coverage(_)
                            | StatementKind::Nop
                    )
                })
                .map(|statement| statement.source_info.span)
                .chain(
                    matches!(
                        data.terminator().kind,
                        TerminatorKind::SwitchInt { .. }
                            | TerminatorKind::Drop { .. }
                            | TerminatorKind::Call { .. }
                            | TerminatorKind::TailCall { .. }
                            | TerminatorKind::Assert { .. }
                            | TerminatorKind::Yield { .. }
                            | TerminatorKind::InlineAsm { .. }
                            | TerminatorKind::Return
                    )
                    .then_some(data.terminator().source_info.span),
                )
                .filter_map(|span| stable_source_range(tcx, span, crate_name).ok())
                .collect::<Vec<_>>();
            sources.sort_by_key(|source| (source.key.clone(), source.start, source.end));
            sources.dedup();
            (block, sources)
        })
        .collect()
}

fn runtime_match_plans<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    body: &Body<'tcx>,
) -> Result<Vec<RuntimeMatchPlan>, String> {
    let Some(obligations) = runtime_body_obligations(tcx, def_id) else {
        return Ok(Vec::new());
    };
    let marker_ordinals = MATCH_ARM_MARKERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&obligations.definition)
        .cloned()
        .unwrap_or_default();
    let marker_blocks = body
        .basic_blocks
        .iter_enumerated()
        .flat_map(|(block, data)| {
            data.statements.iter().filter_map({
                let marker_ordinals = &marker_ordinals;
                move |statement| {
                    let StatementKind::Assign(assignment) = &statement.kind else {
                        return None;
                    };
                    let (destination, _) = &**assignment;
                    let local = destination.as_local()?.as_u32();
                    marker_ordinals
                        .get(&local)
                        .copied()
                        .map(|ordinal| (ordinal, block))
                }
            })
        })
        .fold(
            BTreeMap::<u64, BTreeSet<BasicBlock>>::new(),
            |mut blocks, (ordinal, block)| {
                blocks.entry(ordinal).or_default().insert(block);
                blocks
            },
        );
    let dominators = body.basic_blocks.dominators().clone();
    let block_sources = executable_block_sources(tcx, &obligations.crate_name, body);
    let groups = obligations
        .match_groups
        .values()
        .filter(|group| {
            group.definitions.contains(&obligations.definition)
                && (group.identity.provenance != "synthetic-expansion"
                    || group
                        .arms
                        .iter()
                        .all(|arm| marker_blocks.contains_key(&arm.selected_ordinal)))
        })
        .collect::<Vec<_>>();
    let mut plans = Vec::new();
    for group in groups {
        if group.arms.len() < 2 {
            return Err(format!(
                "match selection group {} has fewer than two reachable arms",
                group.identity.id
            ));
        }
        let mut arms = Vec::new();
        for arm in &group.arms {
            let body_blocks = marker_blocks
                .get(&arm.selected_ordinal)
                .cloned()
                .unwrap_or_else(|| {
                    block_sources
                        .iter()
                        .filter_map(|(block, sources)| {
                            sources
                                .iter()
                                .any(|source| {
                                    source.key == arm.body_source.key
                                        && source.start >= arm.body_source.start
                                        && source.end <= arm.body_source.end
                                })
                                .then_some(*block)
                        })
                        .collect::<BTreeSet<_>>()
                });
            if body_blocks.is_empty() {
                return Err(format!(
                    "match arm {} has no authored MIR body blocks at {}:{}-{}",
                    arm.branch_id, arm.body_source.key, arm.body_source.start, arm.body_source.end
                ));
            }
            let entry_block = body_blocks
                .iter()
                .copied()
                .reduce(|left, right| {
                    nearest_common_dominator(&dominators, left, right).unwrap_or(left)
                })
                .ok_or_else(|| format!("match arm {} has no entry block", arm.branch_id))?;
            if !body_blocks
                .iter()
                .all(|block| dominators.dominates(entry_block, *block))
            {
                return Err(format!(
                    "match arm {} has no unique dominating body entry",
                    arm.branch_id
                ));
            }
            let entry_sources = body.basic_blocks.predecessors()[entry_block]
                .iter()
                .copied()
                .filter(|source| !dominators.dominates(entry_block, *source))
                .collect::<Vec<_>>();
            if entry_sources.is_empty() {
                return Err(format!(
                    "match arm {} entry {:?} has no external incoming edge",
                    arm.branch_id, entry_block
                ));
            }
            arms.push(RuntimeMatchArm {
                branch_id: arm.branch_id.clone(),
                entry_block,
                entry_sources,
                selected_ordinal: arm.selected_ordinal,
            });
        }
        let mut arm_entries = arms.iter().map(|arm| arm.entry_block);
        let Some(first) = arm_entries.next() else {
            return Err(format!(
                "match selection group {} has no arms",
                group.identity.id
            ));
        };
        let start_block = arm_entries.try_fold(first, |left, right| {
            nearest_common_dominator(&dominators, left, right)
        });
        let Some(start_block) = start_block else {
            return Err(format!(
                "match selection group {} has no common MIR dominator",
                group.identity.id
            ));
        };
        if !arms
            .iter()
            .all(|arm| dominators.dominates(start_block, arm.entry_block))
        {
            return Err(format!(
                "match selection group {} has an arm outside its start region",
                group.identity.id
            ));
        }
        plans.push(RuntimeMatchPlan {
            id: group.identity.id.clone(),
            start_block,
            token: None,
            arms,
        });
    }
    Ok(plans)
}

fn runtime_marked_guard_decision_plans<'tcx>(
    tcx: TyCtxt<'tcx>,
    def_id: LocalDefId,
    body: &Body<'tcx>,
) -> Result<Vec<RuntimeDecisionPlan>, String> {
    let Some(obligations) = runtime_body_obligations(tcx, def_id) else {
        return Ok(Vec::new());
    };
    let markers = MATCH_GUARD_MARKERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(&obligations.definition)
        .cloned()
        .unwrap_or_default();
    if markers.is_empty() {
        return Ok(Vec::new());
    }
    let markers_by_local = markers
        .iter()
        .map(|marker| (marker.local, marker))
        .collect::<BTreeMap<_, _>>();
    let mut blocks_by_local = BTreeMap::<u32, Vec<BasicBlock>>::new();
    for (block, data) in body.basic_blocks.iter_enumerated() {
        for statement in &data.statements {
            let StatementKind::Assign(assignment) = &statement.kind else {
                continue;
            };
            let (destination, _) = &**assignment;
            let Some(local) = destination.as_local().map(|local| local.as_u32()) else {
                continue;
            };
            if markers_by_local.contains_key(&local) {
                blocks_by_local.entry(local).or_default().push(block);
            }
        }
    }
    let mut markers_by_decision = BTreeMap::<String, Vec<&MatchGuardConditionMarker>>::new();
    for marker in &markers {
        markers_by_decision
            .entry(marker.decision_id.clone())
            .or_default()
            .push(marker);
    }
    let mut plans = Vec::new();
    for (decision_id, mut decision_markers) in markers_by_decision {
        let decision = obligations.decisions.get(&decision_id).ok_or_else(|| {
            format!("synthetic guard marker references unknown decision {decision_id}")
        })?;
        decision_markers.sort_by_key(|marker| marker.condition_index);
        if decision_markers.len() != decision.conditions.len()
            || decision_markers
                .iter()
                .enumerate()
                .any(|(index, marker)| index != marker.condition_index)
        {
            return Err(format!(
                "synthetic guard {decision_id} has {}/{} ordered condition markers",
                decision_markers.len(),
                decision.conditions.len()
            ));
        }
        let digest = decision_id
            .strip_prefix("rs:decision:")
            .ok_or_else(|| format!("invalid Rust decision ID {decision_id}"))?;
        if digest.len() != 24 {
            return Err(format!("invalid Rust decision digest {digest}"));
        }
        let id_high = u64::from_str_radix(&digest[..16], 16)
            .map_err(|error| format!("invalid Rust decision ID {decision_id}: {error}"))?;
        let id_low = u32::from_str_radix(&digest[16..], 16)
            .map_err(|error| format!("invalid Rust decision ID {decision_id}: {error}"))?;
        let mut conditions = Vec::new();
        for marker in decision_markers {
            let blocks = blocks_by_local
                .get(&marker.local)
                .cloned()
                .unwrap_or_default();
            let [entry_block] = blocks.as_slice() else {
                return Err(format!(
                    "synthetic guard {decision_id} condition {} marker survived in {} MIR blocks",
                    marker.condition_index,
                    blocks.len()
                ));
            };
            let TerminatorKind::SwitchInt { discr, targets } =
                &body.basic_blocks[*entry_block].terminator().kind
            else {
                return Err(format!(
                    "synthetic guard {decision_id} condition {} marker does not precede a switch",
                    marker.condition_index
                ));
            };
            if discr.ty(&body.local_decls, tcx) != tcx.types.bool {
                return Err(format!(
                    "synthetic guard {decision_id} condition {} marker precedes a non-Boolean switch",
                    marker.condition_index
                ));
            }
            let condition = &decision.conditions[marker.condition_index];
            conditions.push(RuntimeDecisionCondition {
                index: marker.condition_index as u64,
                entry_block: *entry_block,
                true_sources: vec![*entry_block],
                false_sources: vec![*entry_block],
                true_target: targets.target_for_value(1),
                false_target: targets.target_for_value(0),
                true_outcome: condition.true_outcome,
                false_outcome: condition.false_outcome,
            });
        }
        plans.push(RuntimeDecisionPlan {
            id: decision_id,
            id_high,
            id_low,
            conditions,
            loop_alternatives: None,
            loop_source: None,
            loop_token: None,
        });
    }
    Ok(plans)
}

fn strip_match_arm_markers(body: &mut Body<'_>, definition: &str) -> Result<(), String> {
    let marker_locals = MATCH_ARM_MARKERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(definition)
        .map(|markers| markers.keys().copied().collect::<BTreeSet<_>>())
        .unwrap_or_default();
    if marker_locals.is_empty() {
        return Ok(());
    }
    let mut removed = 0;
    for data in body.basic_blocks_mut() {
        data.statements.retain(|statement| {
            let StatementKind::Assign(assignment) = &statement.kind else {
                return true;
            };
            let (destination, _) = &**assignment;
            let is_marker = destination
                .as_local()
                .is_some_and(|local| marker_locals.contains(&local.as_u32()));
            removed += usize::from(is_marker);
            !is_marker
        });
    }
    if removed != marker_locals.len() {
        return Err(format!(
            "synthetic match markers in {definition} survived borrow checking {removed}/{} times",
            marker_locals.len()
        ));
    }
    Ok(())
}

fn strip_match_guard_markers(body: &mut Body<'_>, definition: &str) -> Result<(), String> {
    let marker_locals = MATCH_GUARD_MARKERS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(definition)
        .map(|markers| {
            markers
                .iter()
                .map(|marker| marker.local)
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    if marker_locals.is_empty() {
        return Ok(());
    }
    let mut removed = 0;
    for data in body.basic_blocks_mut() {
        data.statements.retain(|statement| {
            let StatementKind::Assign(assignment) = &statement.kind else {
                return true;
            };
            let (destination, _) = &**assignment;
            let is_marker = destination
                .as_local()
                .is_some_and(|local| marker_locals.contains(&local.as_u32()));
            removed += usize::from(is_marker);
            !is_marker
        });
    }
    if removed != marker_locals.len() {
        return Err(format!(
            "synthetic match guard markers in {definition} survived borrow checking {removed}/{} times",
            marker_locals.len()
        ));
    }
    Ok(())
}

fn instrument_runtime_matches<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &mut Body<'tcx>,
    plans: &mut [RuntimeMatchPlan],
    start: LocalDefId,
    hit: LocalDefId,
    unit: rustc_middle::mir::Local,
    span: rustc_span::Span,
) -> Result<(), String> {
    let mut starts = BTreeSet::new();
    for plan in plans.iter() {
        if !starts.insert(plan.start_block.as_u32()) {
            return Err(format!(
                "multiple match selection groups begin in MIR block {:?}",
                plan.start_block
            ));
        }
    }

    // Commit selected arms before splitting selection starts. This preserves
    // nested matches: an outer arm commits on the edge entering the inner
    // match's frame, before the inner scrutinee is selected.
    for plan in plans.iter_mut() {
        let token = body.local_decls.push(LocalDecl::new(tcx.types.u64, span));
        for arm in &plan.arms {
            for source in &arm.entry_sources {
                let cleanup = body.basic_blocks[*source].is_cleanup;
                let bridge = body.basic_blocks_mut().push(runtime_call_block(
                    tcx,
                    hit,
                    [
                        Operand::Copy(Place::from(token)),
                        Operand::const_from_scalar(
                            tcx,
                            tcx.types.u64,
                            Scalar::from_u64(arm.selected_ordinal),
                            span,
                        ),
                    ]
                    .into_iter(),
                    Place::from(unit),
                    arm.entry_block,
                    span,
                    cleanup,
                ));
                let mut replaced = 0;
                body.basic_blocks_mut()[*source]
                    .terminator_mut()
                    .successors_mut(|target| {
                        if *target == arm.entry_block {
                            *target = bridge;
                            replaced += 1;
                        }
                    });
                if replaced == 0 {
                    return Err(format!(
                        "match arm {} entry edge from {:?} was not found",
                        arm.branch_id, source
                    ));
                }
            }
        }
        // Store the local now; its defining call is installed below after all
        // hit edges retain their original source/target identities.
        plan.token = Some(token);
    }

    for plan in plans.iter() {
        let token = plan
            .token
            .ok_or_else(|| format!("match selection group {} has no frame token", plan.id))?;
        let source = plan.start_block;
        let cleanup = body.basic_blocks[source].is_cleanup;
        let original = body.basic_blocks_mut()[source]
            .terminator
            .take()
            .ok_or_else(|| format!("match selection group {} has no start terminator", plan.id))?;
        let continuation = body
            .basic_blocks_mut()
            .push(BasicBlockData::new(Some(original), cleanup));
        let call = runtime_call_block(
            tcx,
            start,
            std::iter::empty(),
            Place::from(token),
            continuation,
            span,
            cleanup,
        );
        body.basic_blocks_mut()[source].terminator = call.terminator;
    }
    Ok(())
}

fn instrument_runtime_for_loops<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &mut Body<'tcx>,
    plans: &mut [RuntimeForLoopPlan],
    start: LocalDefId,
    hit: LocalDefId,
    unit: rustc_middle::mir::Local,
    span: rustc_span::Span,
) -> Result<(), String> {
    for index in 0..plans.len() {
        let dominators = body.basic_blocks.dominators().clone();
        let mut header = dominators
            .immediate_dominator(plans[index].switch_block)
            .ok_or_else(|| format!("for branch {} has no loop header", plans[index].id))?;
        while !matches!(
            body.basic_blocks[header].terminator().kind,
            TerminatorKind::Call { .. }
        ) {
            header = dominators.immediate_dominator(header).ok_or_else(|| {
                format!("for branch {} has no iterator-next call", plans[index].id)
            })?;
        }
        let TerminatorKind::Call {
            destination,
            target: Some(target),
            ..
        } = body.basic_blocks[header].terminator().kind
        else {
            return Err(format!(
                "for branch {} iterator header is not a returning call",
                plans[index].id
            ));
        };
        let ty::Adt(option, _) = destination.ty(&body.local_decls, tcx).ty.kind() else {
            return Err(format!(
                "for branch {} iterator call does not return Option",
                plans[index].id
            ));
        };
        let some = tcx
            .lang_items()
            .option_some_variant()
            .ok_or_else(|| "rustc has no Option::Some lang item".to_owned())?;
        if tcx.parent(some) != option.did()
            || !dominators.dominates(target, plans[index].switch_block)
        {
            return Err(format!(
                "for branch {} iterator call does not lead to its Option switch",
                plans[index].id
            ));
        }
        let cleanup = body.basic_blocks[header].is_cleanup;
        let original = std::mem::replace(
            &mut body.basic_blocks_mut()[header],
            BasicBlockData::new(None, cleanup),
        );
        let iteration_block = body.basic_blocks_mut().push(original);
        for plan in plans.iter_mut() {
            if plan.switch_block == header {
                plan.switch_block = iteration_block;
            }
            if plan.zero_target == header {
                plan.zero_target = iteration_block;
            }
            if plan.entered_target == header {
                plan.entered_target = iteration_block;
            }
        }
        for (predecessor, _) in body.basic_blocks.predecessors()[header]
            .iter()
            .copied()
            .map(|predecessor| (predecessor, dominators.dominates(header, predecessor)))
            .filter(|(_, backedge)| *backedge)
            .collect::<Vec<_>>()
        {
            let predecessor = if predecessor == header {
                iteration_block
            } else {
                predecessor
            };
            let mut replaced = 0;
            body.basic_blocks_mut()[predecessor]
                .terminator_mut()
                .successors_mut(|target| {
                    if *target == header {
                        *target = iteration_block;
                        replaced += 1;
                    }
                });
            if replaced == 0 {
                return Err(format!(
                    "for branch {} lost back edge from {:?}",
                    plans[index].id, predecessor
                ));
            }
        }
        let token = body.local_decls.push(LocalDecl::new(tcx.types.u64, span));
        body.basic_blocks_mut()[header] = runtime_call_block(
            tcx,
            start,
            std::iter::empty(),
            Place::from(token),
            iteration_block,
            span,
            cleanup,
        );
        plans[index].token = Some(token);
    }

    for plan in plans {
        let token = plan
            .token
            .ok_or_else(|| format!("for branch {} has no frame token", plan.id))?;
        for (target, ordinal) in [
            (plan.zero_target, plan.zero_ordinal),
            (plan.entered_target, plan.entered_ordinal),
        ] {
            let cleanup = body.basic_blocks[plan.switch_block].is_cleanup;
            let bridge = body.basic_blocks_mut().push(runtime_call_block(
                tcx,
                hit,
                [
                    Operand::Copy(Place::from(token)),
                    Operand::const_from_scalar(tcx, tcx.types.u64, Scalar::from_u64(ordinal), span),
                ]
                .into_iter(),
                Place::from(unit),
                target,
                span,
                cleanup,
            ));
            let mut replaced = 0;
            body.basic_blocks_mut()[plan.switch_block]
                .terminator_mut()
                .successors_mut(|edge| {
                    if *edge == target {
                        *edge = bridge;
                        replaced += 1;
                    }
                });
            if replaced != 1 {
                return Err(format!(
                    "for branch {} target {:?} replacement count was {replaced}",
                    plan.id, target
                ));
            }
        }
    }
    Ok(())
}

fn instrument_runtime_loop_frames<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &mut Body<'tcx>,
    plans: &mut [RuntimeDecisionPlan],
    start: LocalDefId,
    span: rustc_span::Span,
) -> Result<(), String> {
    let loop_indices = plans
        .iter()
        .enumerate()
        .filter_map(|(index, plan)| plan.loop_alternatives.map(|_| index))
        .collect::<Vec<_>>();
    for index in loop_indices {
        let condition_entry = plans[index]
            .conditions
            .first()
            .ok_or_else(|| format!("loop decision {} has no conditions", plans[index].id))?
            .entry_block;
        let loop_source = plans[index]
            .loop_source
            .as_ref()
            .ok_or_else(|| format!("loop decision {} has no source range", plans[index].id))?;
        let dominators = body.basic_blocks.dominators().clone();
        let crate_name = tcx.crate_name(rustc_span::def_id::LOCAL_CRATE).to_string();
        let block_is_in_loop = |block: BasicBlock| {
            body.basic_blocks[block]
                .statements
                .iter()
                .map(|statement| statement.source_info.span)
                .chain(std::iter::once(
                    body.basic_blocks[block].terminator().source_info.span,
                ))
                .filter_map(|span| stable_source_range(tcx, span, &crate_name).ok())
                .any(|source| {
                    source.key == loop_source.key
                        && source.start >= loop_source.start
                        && source.end <= loop_source.end
                })
        };
        let back_edges_for = |header: BasicBlock| {
            body.basic_blocks.predecessors()[header]
                .iter()
                .copied()
                .filter(|predecessor| dominators.dominates(header, *predecessor))
                .collect::<Vec<_>>()
        };
        let mut header = condition_entry;
        let mut back_edges = back_edges_for(header);
        while back_edges.is_empty()
            && let Some(parent) = dominators.immediate_dominator(header)
            && block_is_in_loop(parent)
        {
            header = parent;
            back_edges = back_edges_for(header);
        }
        let cleanup = body.basic_blocks[header].is_cleanup;
        let original = std::mem::replace(
            &mut body.basic_blocks_mut()[header],
            BasicBlockData::new(None, cleanup),
        );
        let condition_block = body.basic_blocks_mut().push(original);
        for predecessor in back_edges {
            let predecessor = if predecessor == header {
                condition_block
            } else {
                predecessor
            };
            let mut replaced = 0;
            body.basic_blocks_mut()[predecessor]
                .terminator_mut()
                .successors_mut(|target| {
                    if *target == header {
                        *target = condition_block;
                        replaced += 1;
                    }
                });
            if replaced == 0 {
                return Err(format!(
                    "loop decision {} lost back edge from {:?}",
                    plans[index].id, predecessor
                ));
            }
        }
        for plan in plans.iter_mut() {
            for condition in &mut plan.conditions {
                let source_inside_loop = dominators.dominates(header, condition.entry_block);
                if source_inside_loop && condition.true_target == header {
                    condition.true_target = condition_block;
                }
                if source_inside_loop && condition.false_target == header {
                    condition.false_target = condition_block;
                }
                for source in condition
                    .true_sources
                    .iter_mut()
                    .chain(&mut condition.false_sources)
                {
                    if *source == header {
                        *source = condition_block;
                    }
                }
                if condition.entry_block == header {
                    condition.entry_block = condition_block;
                }
            }
        }
        let token = body.local_decls.push(LocalDecl::new(tcx.types.u64, span));
        body.basic_blocks_mut()[header] = runtime_call_block(
            tcx,
            start,
            std::iter::empty(),
            Place::from(token),
            condition_block,
            span,
            cleanup,
        );
        plans[index].loop_token = Some(token);
    }
    Ok(())
}

fn strip_native_coverage(body: &mut Body<'_>) {
    for data in body.basic_blocks_mut() {
        data.statements
            .retain(|statement| !matches!(statement.kind, StatementKind::Coverage(_)));
    }
    body.coverage_info_hi = None;
    body.function_coverage_info = None;
}

fn instrument_runtime_decisions<'tcx>(
    tcx: TyCtxt<'tcx>,
    body: &mut Body<'tcx>,
    plans: &[RuntimeDecisionPlan],
    runtime: DecisionRuntime,
    span: rustc_span::Span,
) -> Result<(), String> {
    let mut starts = BTreeSet::new();
    for plan in plans {
        let Some(first) = plan.conditions.first() else {
            return Err(format!("decision {} has no conditions", plan.id));
        };
        if !starts.insert(first.entry_block.as_u32()) {
            return Err(format!(
                "multiple decisions begin in MIR block {:?}; nested/shared starts require an explicit ordering",
                first.entry_block
            ));
        }
        let token = body.local_decls.push(LocalDecl::new(tcx.types.u64, span));
        for mapped in &plan.conditions {
            for (value, sources, target, outcome) in [
                (
                    true,
                    mapped.true_sources.as_slice(),
                    mapped.true_target,
                    mapped.true_outcome,
                ),
                (
                    false,
                    mapped.false_sources.as_slice(),
                    mapped.false_target,
                    mapped.false_outcome,
                ),
            ] {
                for source in sources {
                    let cleanup = body.basic_blocks[*source].is_cleanup;
                    let mut continuation = target;
                    if let Some(outcome) = outcome {
                        if let (Some(token), Some((zero, entered))) =
                            (plan.loop_token, plan.loop_alternatives)
                        {
                            let Some(branch_hit) = runtime.branch_hit else {
                                return Err(format!(
                                    "loop decision {} has no branch-hit runtime",
                                    plan.id
                                ));
                            };
                            continuation = body.basic_blocks_mut().push(runtime_call_block(
                                tcx,
                                branch_hit,
                                [
                                    Operand::Copy(Place::from(token)),
                                    Operand::const_from_scalar(
                                        tcx,
                                        tcx.types.u64,
                                        Scalar::from_u64(if outcome { entered } else { zero }),
                                        span,
                                    ),
                                ]
                                .into_iter(),
                                Place::from(runtime.unit),
                                continuation,
                                span,
                                cleanup,
                            ));
                        }
                        continuation = body.basic_blocks_mut().push(runtime_call_block(
                            tcx,
                            runtime.finish,
                            [
                                Operand::Copy(Place::from(token)),
                                Operand::const_from_scalar(
                                    tcx,
                                    tcx.types.bool,
                                    Scalar::from_bool(outcome),
                                    span,
                                ),
                            ]
                            .into_iter(),
                            Place::from(runtime.unit),
                            continuation,
                            span,
                            cleanup,
                        ));
                    }
                    let bridge = body.basic_blocks_mut().push(runtime_call_block(
                        tcx,
                        runtime.condition,
                        [
                            Operand::Copy(Place::from(token)),
                            Operand::const_from_scalar(
                                tcx,
                                tcx.types.u64,
                                Scalar::from_u64(mapped.index),
                                span,
                            ),
                            Operand::const_from_scalar(
                                tcx,
                                tcx.types.bool,
                                Scalar::from_bool(value),
                                span,
                            ),
                        ]
                        .into_iter(),
                        Place::from(runtime.unit),
                        continuation,
                        span,
                        cleanup,
                    ));
                    let mut replaced = 0;
                    body.basic_blocks_mut()[*source]
                        .terminator_mut()
                        .successors_mut(|edge| {
                            if *edge == target {
                                *edge = bridge;
                                replaced += 1;
                            }
                        });
                    if replaced == 0 {
                        return Err(format!(
                            "decision {} condition {} {:?} edge from {:?} was not found",
                            plan.id, mapped.index, value, source
                        ));
                    }
                }
            }
        }

        let source = first.entry_block;
        let cleanup = body.basic_blocks[source].is_cleanup;
        let original = body.basic_blocks_mut()[source]
            .terminator
            .take()
            .ok_or_else(|| format!("decision {} start block has no terminator", plan.id))?;
        let continuation = body
            .basic_blocks_mut()
            .push(BasicBlockData::new(Some(original), cleanup));
        let call = runtime_call_block(
            tcx,
            runtime.start,
            [
                Operand::const_from_scalar(
                    tcx,
                    tcx.types.u64,
                    Scalar::from_u64(plan.id_high),
                    span,
                ),
                Operand::const_from_scalar(tcx, tcx.types.u32, Scalar::from_u32(plan.id_low), span),
                Operand::const_from_scalar(
                    tcx,
                    tcx.types.u64,
                    Scalar::from_u64(plan.conditions.len() as u64),
                    span,
                ),
            ]
            .into_iter(),
            Place::from(token),
            continuation,
            span,
            cleanup,
        );
        body.basic_blocks_mut()[source].terminator = call.terminator;
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct DecisionRuntime {
    start: LocalDefId,
    condition: LocalDefId,
    finish: LocalDefId,
    branch_hit: Option<LocalDefId>,
    unit: rustc_middle::mir::Local,
}

fn optimized_mir_with_probe<'tcx>(tcx: TyCtxt<'tcx>, def_id: LocalDefId) -> &'tcx Body<'tcx> {
    let original = ORIGINAL_OPTIMIZED_MIR
        .get()
        .expect("original optimized_mir provider");
    let body = original(tcx, def_id);
    if env::var_os(INSTRUMENT_MIR).is_none() {
        return body;
    }
    let definition = tcx.def_path_str(def_id);
    let mut decision_plans = runtime_decision_plans(tcx, def_id, body).unwrap_or_else(|error| {
        tcx.dcx().fatal(format!(
            "Supercov could not bind Rust decision probes in {definition}: {error}"
        ))
    });
    let probe_id = probe_id_for(tcx, def_id, &definition);
    let context_id = context_id_for(tcx, def_id, &definition);
    let probe_function = probe_id.and_then(|_| find_runtime_function(tcx, PROBE_FUNCTION));
    let enter_context = context_id.and_then(|_| find_runtime_function(tcx, ENTER_CONTEXT_FUNCTION));
    let exit_context = context_id.and_then(|_| find_runtime_function(tcx, EXIT_CONTEXT_FUNCTION));
    let start_decision = (!decision_plans.is_empty())
        .then(|| find_runtime_function(tcx, START_DECISION_FUNCTION))
        .flatten();
    let record_condition = (!decision_plans.is_empty())
        .then(|| find_runtime_function(tcx, RECORD_CONDITION_FUNCTION))
        .flatten();
    let finish_decision = (!decision_plans.is_empty())
        .then(|| find_runtime_function(tcx, FINISH_DECISION_FUNCTION))
        .flatten();
    let has_loop_plans = decision_plans
        .iter()
        .any(|plan| plan.loop_alternatives.is_some());
    let start_branch = has_loop_plans
        .then(|| find_runtime_function(tcx, START_BRANCH_FUNCTION))
        .flatten();
    let hit_branch = has_loop_plans
        .then(|| find_runtime_function(tcx, HIT_BRANCH_FUNCTION))
        .flatten();
    if probe_id.is_some() != probe_function.is_some()
        || context_id.is_some() != (enter_context.is_some() && exit_context.is_some())
        || (!decision_plans.is_empty()
            && (start_decision.is_none()
                || record_condition.is_none()
                || finish_decision.is_none()))
        || has_loop_plans != (start_branch.is_some() && hit_branch.is_some())
    {
        tcx.dcx().fatal(format!(
            "Supercov injected runtime functions are incomplete while instrumenting {definition}"
        ));
    }

    let mut instrumented = body.clone();
    strip_native_coverage(&mut instrumented);
    let span = tcx.def_span(def_id);
    if probe_id.is_none() && context_id.is_none() && decision_plans.is_empty() {
        return tcx.arena.alloc(instrumented);
    }
    let unit = instrumented
        .local_decls
        .push(LocalDecl::new(tcx.types.unit, span));
    if let Some(start_branch) = start_branch
        && let Err(error) = instrument_runtime_loop_frames(
            tcx,
            &mut instrumented,
            &mut decision_plans,
            start_branch,
            span,
        )
    {
        tcx.dcx().fatal(format!(
            "Supercov could not inject Rust loop frames in {definition}: {error}"
        ));
    }
    if let (Some(start), Some(condition), Some(finish)) =
        (start_decision, record_condition, finish_decision)
        && let Err(error) = instrument_runtime_decisions(
            tcx,
            &mut instrumented,
            &decision_plans,
            DecisionRuntime {
                start,
                condition,
                finish,
                branch_hit: hit_branch,
                unit,
            },
            span,
        )
    {
        tcx.dcx().fatal(format!(
            "Supercov could not inject Rust decision probes in {definition}: {error}"
        ));
    }
    let previous_context = context_id.map(|_| {
        instrumented
            .local_decls
            .push(LocalDecl::new(tcx.types.u64, span))
    });
    if let (Some(previous), Some(exit)) = (previous_context, exit_context) {
        let continuing_unwinds = instrumented
            .basic_blocks
            .iter_enumerated()
            .filter_map(|(block, data)| {
                matches!(data.terminator().unwind(), Some(UnwindAction::Continue)).then_some(block)
            })
            .collect::<Vec<_>>();
        let exits = instrumented
            .basic_blocks
            .iter_enumerated()
            .filter_map(|(block, data)| {
                matches!(
                    data.terminator().kind,
                    TerminatorKind::Return | TerminatorKind::UnwindResume
                )
                .then_some((block, data.is_cleanup))
            })
            .collect::<Vec<_>>();
        for (block, cleanup) in exits {
            let original = instrumented.basic_blocks[block].terminator().clone();
            let continuation = instrumented
                .basic_blocks_mut()
                .push(BasicBlockData::new(Some(original), cleanup));
            instrumented.basic_blocks_mut()[block] = runtime_call_block(
                tcx,
                exit,
                [Operand::Copy(Place::from(previous))].into_iter(),
                Place::from(unit),
                continuation,
                span,
                cleanup,
            );
        }
        if !continuing_unwinds.is_empty() {
            let resume = instrumented.basic_blocks_mut().push(BasicBlockData::new(
                Some(Terminator {
                    source_info: SourceInfo::outermost(span),
                    kind: TerminatorKind::UnwindResume,
                }),
                true,
            ));
            let cleanup = instrumented.basic_blocks_mut().push(runtime_call_block(
                tcx,
                exit,
                [Operand::Copy(Place::from(previous))].into_iter(),
                Place::from(unit),
                resume,
                span,
                true,
            ));
            for block in continuing_unwinds {
                *instrumented.basic_blocks_mut()[block]
                    .terminator_mut()
                    .unwind_mut()
                    .expect("collected unwind action") = UnwindAction::Cleanup(cleanup);
            }
        }
    }
    let continuation = {
        let original_start =
            instrumented.basic_blocks_mut()[rustc_middle::mir::START_BLOCK].clone();
        instrumented.basic_blocks_mut().push(original_start)
    };
    let mut entry = continuation;
    if let (Some(probe_id), Some(probe_function)) = (probe_id, probe_function) {
        let block = runtime_call_block(
            tcx,
            probe_function,
            [Operand::const_from_scalar(
                tcx,
                tcx.types.u64,
                Scalar::from_u64(probe_id),
                span,
            )]
            .into_iter(),
            Place::from(unit),
            entry,
            span,
            false,
        );
        if context_id.is_some() {
            entry = instrumented.basic_blocks_mut().push(block);
        } else {
            instrumented.basic_blocks_mut()[rustc_middle::mir::START_BLOCK] = block;
        }
    }
    if let (Some(context_id), Some(enter), Some(previous)) =
        (context_id, enter_context, previous_context)
    {
        instrumented.basic_blocks_mut()[rustc_middle::mir::START_BLOCK] = runtime_call_block(
            tcx,
            enter,
            [Operand::const_from_scalar(
                tcx,
                tcx.types.u64,
                Scalar::from_u64(context_id),
                span,
            )]
            .into_iter(),
            Place::from(previous),
            entry,
            span,
            false,
        );
    }
    tcx.arena.alloc(instrumented)
}

fn find_runtime_function(tcx: TyCtxt<'_>, suffix: &str) -> Option<LocalDefId> {
    tcx.hir_free_items()
        .map(|item| item.owner_id.def_id)
        .find(|item| tcx.def_path_str(*item).ends_with(suffix))
}

fn runtime_call_block<'tcx>(
    tcx: TyCtxt<'tcx>,
    function: LocalDefId,
    arguments: impl Iterator<Item = Operand<'tcx>>,
    destination: Place<'tcx>,
    target: rustc_middle::mir::BasicBlock,
    span: rustc_span::Span,
    cleanup: bool,
) -> BasicBlockData<'tcx> {
    BasicBlockData::new(
        Some(Terminator {
            source_info: SourceInfo::outermost(span),
            kind: TerminatorKind::Call {
                func: Operand::function_handle(tcx, function.to_def_id(), [], span),
                args: arguments
                    .map(|node| Spanned {
                        node,
                        span: DUMMY_SP,
                    })
                    .collect(),
                destination,
                target: Some(target),
                unwind: UnwindAction::Continue,
                call_source: CallSource::Misc,
                fn_span: span,
            },
        }),
        cleanup,
    )
}

fn probe_id_for(tcx: TyCtxt<'_>, def_id: LocalDefId, definition: &str) -> Option<u64> {
    let targeted = ["authored", "fallible", "drop_order", "panic_path"]
        .iter()
        .any(|suffix| definition.ends_with(suffix));
    if !targeted {
        return None;
    }
    let crate_name = tcx.crate_name(rustc_span::def_id::LOCAL_CRATE).to_string();
    match function_identity(tcx, def_id.to_def_id(), tcx.def_span(def_id), &crate_name) {
        Ok(identity) => Some(identity.probe_ordinal),
        Err(error) => tcx.dcx().fatal(format!(
            "Supercov could not bind runtime probe {definition} to its manifest: {error}"
        )),
    }
}

fn context_id_for(tcx: TyCtxt<'_>, def_id: LocalDefId, definition: &str) -> Option<u64> {
    if matches!(tcx.def_kind(def_id), DefKind::Fn)
        && let Some(test_name) = tcx.hir_body_owners().find_map(|owner| {
            rustc_hir::find_attr!(tcx, owner, RustcTestMarker(name) => *name)
                .filter(|name| name.as_str() == definition)
        })
    {
        return Some(test_context_id(test_name.as_str()));
    }
    for (suffix, context_id) in [("context_normal_scope", 303), ("context_panic_scope", 404)] {
        if definition.ends_with(suffix) {
            return Some(context_id);
        }
    }
    None
}

fn test_context_id(test_name: &str) -> u64 {
    let mut value = 0xcbf2_9ce4_8422_2325_u64;
    for byte in b"supercov-rust-test-v1\0"
        .iter()
        .copied()
        .chain(test_name.bytes())
    {
        value ^= u64::from(byte);
        value = value.wrapping_mul(0x0000_0100_0000_01b3);
    }
    if matches!(value, 0 | u64::MAX) {
        value ^ 0xa5a5_a5a5_a5a5_a5a5
    } else {
        value
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if character <= '\u{001f}' => {
                use std::fmt::Write as _;
                write!(&mut escaped, "\\u{:04x}", character as u32)
                    .expect("writing to a string cannot fail");
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn json_string(value: Option<&str>) -> String {
    value.map_or_else(|| "null".into(), |value| format!("\"{}\"", escape(value)))
}

fn json_strings(values: &[String]) -> String {
    let values = values
        .iter()
        .map(|value| format!("\"{}\"", escape(value)))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

fn json_usizes(values: &[usize]) -> String {
    let values = values
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

fn main() {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if env::args()
        .next()
        .and_then(|argument| {
            PathBuf::from(argument)
                .file_name()
                .map(|name| name.to_owned())
        })
        .is_some_and(|name| name == "supercov-rustdoc-backend-spike")
    {
        launch_rustdoc(&args);
    }
    if env::var_os(RUSTDOC_LAUNCHED).is_some() {
        let role = doctest_role(&args);
        let _ = DOCTEST_ROLE.set(role);
        if role != "merged-runner" {
            strip_injected_rustdoc_unstable_option(&mut args);
            // SAFETY: the compiler companion has not created any threads yet.
            unsafe { env::remove_var("RUSTC_BOOTSTRAP") };
        }
    }
    if env::var_os(INSTRUMENT_MIR).is_some() {
        // Ask the exact rustc companion to retain its THIR-to-MIR branch
        // regions. optimized_mir_with_probe translates those regions into
        // Supercov probes and removes every native coverage artifact before
        // codegen. The exact-version no-profiler-runtime switch prevents rustc
        // from injecting LLVM's profiler crate; the spike also gates absence
        // of native profile output and symbols in the linked executable.
        // SAFETY: the compiler companion has not created any threads yet.
        unsafe { env::set_var("RUSTC_BOOTSTRAP", "1") };
        args.push("-Cinstrument-coverage".into());
        args.push("-Zcoverage-options=branch".into());
        args.push("-Zno-profiler-runtime".into());
        args.push("--cfg=supercov_spike_instrumented".into());
        args.push("--check-cfg=cfg(supercov_spike_instrumented)".into());
    }
    if env::var_os(INSTRUMENT_CTFE).is_some() {
        let subscriber = rustc_log::tracing_subscriber::Registry::default().with(CtfeLayer);
        rustc_log::tracing::subscriber::set_global_default(subscriber)
            .expect("install CTFE event observer");
    }
    let mut callbacks = ProbeCallbacks;
    rustc_driver::run_compiler(&args, &mut callbacks);
    if env::var_os(INSTRUMENT_CTFE).is_some() {
        let events = CTFE_EVENTS.lock().expect("CTFE events lock");
        write_ctfe_events(&args, &events);
    }
}

fn launch_rustdoc(args: &[String]) -> ! {
    let rustdoc = env::var_os(REAL_RUSTDOC).expect("exact rustdoc path");
    let companion = env::var_os(COMPANION_PATH).expect("compiler companion path");
    let status = Command::new(rustdoc)
        .args(args)
        .arg("-Zunstable-options")
        .arg("--test-builder-wrapper")
        .arg(companion)
        .env("RUSTC_BOOTSTRAP", "1")
        .env(RUSTDOC_LAUNCHED, "1")
        .status()
        .expect("launch exact rustdoc");
    std::process::exit(status.code().unwrap_or(1));
}

fn doctest_role(args: &[String]) -> &'static str {
    if env::var_os("UNSTABLE_RUSTDOC_TEST_PATH").is_some() {
        "standalone"
    } else if args
        .iter()
        .any(|argument| argument.contains("doctest_runner_"))
    {
        "merged-runner"
    } else if args
        .iter()
        .any(|argument| argument.contains("doctest_bundle_"))
    {
        "merged-bundle"
    } else {
        "unknown"
    }
}

fn strip_injected_rustdoc_unstable_option(args: &mut [String]) {
    for argument in args {
        let Some(path) = argument.strip_prefix('@') else {
            continue;
        };
        let Ok(contents) = fs::read_to_string(path) else {
            continue;
        };
        let filtered = contents
            .lines()
            .filter(|line| *line != "-Zunstable-options")
            .collect::<Vec<_>>()
            .join("\n");
        if filtered == contents || filtered.len() == contents.len() {
            continue;
        }
        let filtered_path = format!("{path}.supercov-{}", std::process::id());
        let Ok(mut output) = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&filtered_path)
        else {
            continue;
        };
        if output.write_all(filtered.as_bytes()).is_ok() && output.flush().is_ok() {
            *argument = format!("@{filtered_path}");
        }
    }
}

fn write_ctfe_events(args: &[String], events: &[u64]) {
    if events.is_empty() {
        return;
    }
    let Ok(directory) = env::var(OUTPUT_DIRECTORY) else {
        return;
    };
    let directory = PathBuf::from(directory);
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    let crate_name = args
        .windows(2)
        .find_map(|pair| (pair[0] == "--crate-name").then_some(pair[1].as_str()))
        .unwrap_or("unknown");
    let output = directory.join(format!(
        "{}-{}-ctfe.jsonl",
        std::process::id(),
        sanitize(crate_name)
    ));
    let Ok(mut output) = OpenOptions::new().create_new(true).write(true).open(output) else {
        return;
    };
    for marker in events {
        let ordinal = marker & 0x7fff;
        let observation_kind = if marker & CTFE_EDGE_MARKER_OFFSET == 0 {
            "block"
        } else {
            "edge"
        };
        let record = format!(
            "{{\"crate\":\"{}\",\"kind\":\"ctfe-marker\",\"marker\":{},\"observationKind\":\"{}\",\"ordinal\":{}}}\n",
            escape(crate_name),
            marker,
            observation_kind,
            ordinal,
        );
        let _ = output.write_all(record.as_bytes());
    }
    let _ = output.flush();
}
