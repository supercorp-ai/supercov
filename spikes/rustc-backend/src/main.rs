#![feature(rustc_private)]

extern crate rustc_ast;
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
        BasicBlockData, Body, CallSource, LocalDecl, Operand, Place, Rvalue, SourceInfo, Statement,
        StatementKind, Terminator, TerminatorKind, UnwindAction, interpret::Scalar,
    },
    ty::TyCtxt,
    util::Providers,
};
use rustc_session::Session;
use rustc_span::{DUMMY_SP, FileName, def_id::LocalDefId, source_map::Spanned};

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
const CTFE_EVENT_TARGET: &str = "rustc_const_eval::interpret::step";
const CTFE_MARKER_PREFIX: u64 = 0x5355_5045_5243_0000;
const CTFE_EDGE_MARKER_OFFSET: u64 = 0x8000;
const RUNTIME_TEMPLATE: &str =
    include_str!("../../../crates/supercov-engine/runtime-assets/rust-mmap-runtime.rs");

type OptimizedMirProvider = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Body<'tcx>;
type MirForCtfeProvider = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Body<'tcx>;

static ORIGINAL_OPTIMIZED_MIR: OnceLock<OptimizedMirProvider> = OnceLock::new();
static ORIGINAL_MIR_FOR_CTFE: OnceLock<MirForCtfeProvider> = OnceLock::new();
static CTFE_EVENTS: Mutex<Vec<u64>> = Mutex::new(Vec::new());
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
    alternatives: Vec<BranchAlternativeObligation>,
    definitions: Vec<String>,
}

#[derive(Debug, PartialEq, Eq)]
struct DecisionCondition {
    source: StableSourceRange,
    text: String,
}

#[derive(Debug)]
struct DecisionObligation {
    identity: StableObligationIdentity,
    decision_kind: &'static str,
    conditions: Vec<DecisionCondition>,
    definitions: Vec<String>,
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
    limitations: &'a mut BTreeSet<String>,
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

    fn record_if(&mut self, expression: &'tcx hir::Expr<'tcx>, condition: &'tcx hir::Expr<'tcx>) {
        let mut atomic = Vec::new();
        flatten_boolean_conditions(condition, &mut atomic);
        let has_let = atomic
            .iter()
            .any(|condition| matches!(condition.kind, hir::ExprKind::Let(_)));
        let decision_kind = if has_let && atomic.len() > 1 {
            "let-chain"
        } else if has_let {
            "if-let"
        } else {
            "if"
        };
        let conditions = atomic
            .into_iter()
            .filter_map(|condition| {
                match stable_source_range(self.tcx, condition.span, self.crate_name) {
                    Ok(source) if source.owned => Some(DecisionCondition {
                        text: if condition.span.from_expansion()
                            && stable_source_range(
                                self.tcx,
                                condition.span.source_callsite(),
                                self.crate_name,
                            )
                            .is_ok_and(|callsite| callsite == source)
                        {
                            rustc_hir_pretty::expr_to_string(&self.tcx, condition)
                        } else {
                            self.tcx
                                .sess
                                .source_map()
                                .span_to_snippet(condition.span)
                                .unwrap_or_else(|_| "<source unavailable>".into())
                        },
                        source,
                    }),
                    Ok(source) => {
                        self.limitations.insert(format!(
                            "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: condition: unowned {} source {}",
                            self.definition, source.class, source.key
                        ));
                        None
                    }
                    Err(error) => {
                        self.limitations.insert(format!(
                            "RUST_SOURCE_IDENTITY_UNRESOLVED: {}: condition: {error}",
                            self.definition
                        ));
                        None
                    }
                }
            })
            .collect::<Vec<_>>();
        if conditions.is_empty() {
            return;
        }
        let Some(decision) = self.identity("decision", condition.span, decision_kind) else {
            return;
        };
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

        let Some(branch) = self.identity("branch", expression.span, decision_kind) else {
            return;
        };
        let Some(true_alternative) = self.identity(
            "branch-alternative",
            expression.span,
            &format!("{decision_kind}:true"),
        ) else {
            return;
        };
        let Some(false_alternative) = self.identity(
            "branch-alternative",
            expression.span,
            &format!("{decision_kind}:false"),
        ) else {
            return;
        };
        match self.branches.get_mut(&branch.id) {
            Some(existing) if existing.identity.canonical != branch.canonical => {
                self.tcx.dcx().fatal(format!(
                    "Supercov Rust obligation ID collision for {}",
                    branch.id
                ))
            }
            Some(existing)
                if existing.branch_kind != decision_kind
                    || existing.alternatives
                        != [
                            BranchAlternativeObligation {
                                identity: true_alternative.clone(),
                                label: "condition true",
                            },
                            BranchAlternativeObligation {
                                identity: false_alternative.clone(),
                                label: "condition false",
                            },
                        ] =>
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
                        branch_kind: decision_kind,
                        alternatives: vec![
                            BranchAlternativeObligation {
                                identity: true_alternative,
                                label: "condition true",
                            },
                            BranchAlternativeObligation {
                                identity: false_alternative,
                                label: "condition false",
                            },
                        ],
                        definitions: vec![self.definition.clone()],
                    },
                );
            }
        }
    }
}

fn flatten_boolean_conditions<'tcx>(
    expression: &'tcx hir::Expr<'tcx>,
    output: &mut Vec<&'tcx hir::Expr<'tcx>>,
) {
    match expression.kind {
        hir::ExprKind::Binary(operator, left, right)
            if matches!(
                operator.node,
                rustc_ast::BinOpKind::And | rustc_ast::BinOpKind::Or
            ) =>
        {
            flatten_boolean_conditions(left, output);
            flatten_boolean_conditions(right, output);
        }
        _ => output.push(expression),
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
        if let hir::ExprKind::If(condition, _, _) = expression.kind {
            self.record_if(expression, condition);
        }
        intravisit::walk_expr(self, expression);
    }
}

fn manifest_json(
    crate_name: &str,
    points: &BTreeMap<String, PointObligation>,
    branches: &BTreeMap<String, BranchObligation>,
    decisions: &BTreeMap<String, DecisionObligation>,
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
                "{{\"id\":\"{}\",\"kind\":\"{}\",\"sourceKey\":\"{}\",\"start\":{},\"end\":{},\"provenance\":\"{}\",\"probeOrdinal\":\"{}\",\"definitions\":{},\"alternatives\":[{}],\"canonical\":\"{}\"}}",
                escape(&branch.identity.id),
                branch.branch_kind,
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
    let limitations = limitations
        .iter()
        .map(|limitation| format!("\"{}\"", escape(limitation)))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\"schema\":\"supercov-rust-manifest-candidate-v1\",\"model\":\"rust-source-v1\",\"crate\":\"{}\",\"measurementComplete\":false,\"points\":[{}],\"branches\":[{}],\"decisions\":[{}],\"limitations\":[{}]}}\n",
        escape(crate_name),
        points,
        branches,
        decisions,
        limitations
    )
}

fn reject_probe_ordinal_collisions(
    tcx: TyCtxt<'_>,
    points: &BTreeMap<String, PointObligation>,
    branches: &BTreeMap<String, BranchObligation>,
    decisions: &BTreeMap<String, DecisionObligation>,
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
        let mut manifest_limitations = BTreeSet::from([
            "RUST_MANIFEST_CANDIDATE_IF_SLICE_ONLY: loop, match, let-else, try, assertion, CTFE and doctest obligation/probe mappings are not emitted yet".to_owned(),
        ]);

        for owner in tcx.hir_body_owners() {
            let def_id = owner.to_def_id();
            let span = tcx.def_span(def_id);
            let callsite = span.source_callsite();
            let kind = tcx.def_kind(def_id);
            let mir = if matches!(
                kind,
                DefKind::Const { .. }
                    | DefKind::AssocConst { .. }
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
                    limitations: &mut manifest_limitations,
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
        let limitations = manifest_limitations.into_iter().collect::<Vec<_>>();
        reject_probe_ordinal_collisions(tcx, &points, &branches, &decisions);
        let manifest = manifest_json(
            &crate_name_string,
            &points,
            &branches,
            &decisions,
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
    providers.queries.optimized_mir = optimized_mir_with_probe;
    providers.queries.mir_for_ctfe = mir_for_ctfe_with_markers;
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

fn optimized_mir_with_probe<'tcx>(tcx: TyCtxt<'tcx>, def_id: LocalDefId) -> &'tcx Body<'tcx> {
    let original = ORIGINAL_OPTIMIZED_MIR
        .get()
        .expect("original optimized_mir provider");
    let body = original(tcx, def_id);
    if env::var_os(INSTRUMENT_MIR).is_none() {
        return body;
    }
    let definition = tcx.def_path_str(def_id);
    let probe_id = probe_id_for(tcx, def_id, &definition);
    let context_id = context_id_for(tcx, def_id, &definition);
    if probe_id.is_none() && context_id.is_none() {
        return body;
    }
    let probe_function = probe_id.and_then(|_| find_runtime_function(tcx, PROBE_FUNCTION));
    let enter_context = context_id.and_then(|_| find_runtime_function(tcx, ENTER_CONTEXT_FUNCTION));
    let exit_context = context_id.and_then(|_| find_runtime_function(tcx, EXIT_CONTEXT_FUNCTION));
    if probe_id.is_some() != probe_function.is_some()
        || context_id.is_some() != (enter_context.is_some() && exit_context.is_some())
    {
        return body;
    }

    let mut instrumented = body.clone();
    let span = tcx.def_span(def_id);
    let unit = instrumented
        .local_decls
        .push(LocalDecl::new(tcx.types.unit, span));
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
