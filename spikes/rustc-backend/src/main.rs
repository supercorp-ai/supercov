#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_log;
extern crate rustc_middle;
extern crate rustc_parse;
extern crate rustc_session;
extern crate rustc_span;

use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    process::Command,
    sync::{Mutex, OnceLock},
};

use rustc_driver::{Callbacks, Compilation};
use rustc_errors::ErrorGuaranteed;
use rustc_hir::def::DefKind;
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

const OUTPUT_DIRECTORY: &str = "SUPERCOV_RUSTC_SPIKE_OUTPUT";
const INSTRUMENT_MIR: &str = "SUPERCOV_RUSTC_SPIKE_INSTRUMENT_MIR";
const INSTRUMENT_CTFE: &str = "SUPERCOV_RUSTC_SPIKE_INSTRUMENT_CTFE";
const REAL_RUSTDOC: &str = "SUPERCOV_RUSTC_SPIKE_REAL_RUSTDOC";
const COMPANION_PATH: &str = "SUPERCOV_RUSTC_SPIKE_COMPANION_PATH";
const RUSTDOC_LAUNCHED: &str = "SUPERCOV_RUSTC_SPIKE_RUSTDOC_LAUNCHED";
const PROBE_FUNCTION: &str = "__supercov_spike_runtime::ordinal_hit";
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
        if fs::create_dir_all(&directory).is_err() {
            return Compilation::Continue;
        }

        let crate_name = tcx.crate_name(rustc_span::def_id::LOCAL_CRATE);
        let output = directory.join(format!(
            "{}-{}.jsonl",
            std::process::id(),
            sanitize(&crate_name.to_string())
        ));
        let Ok(mut output) = OpenOptions::new().create_new(true).write(true).open(output) else {
            return Compilation::Continue;
        };
        let source_map = tcx.sess.source_map();
        let doctest_role = DOCTEST_ROLE.get().copied();
        let doctest_path = env::var("UNSTABLE_RUSTDOC_TEST_PATH").ok();
        let doctest_line = env::var("UNSTABLE_RUSTDOC_TEST_LINE").ok();

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
            let record = format!(
                "{{\"crate\":\"{}\",\"definition\":\"{}\",\"kind\":\"{:?}\",\"span\":\"{}\",\"callsite\":\"{}\",\"expanded\":{},\"mirBlocks\":{},\"mirSpans\":{},\"mirAuthoredLines\":{},\"sourceSnippet\":{},\"bodySnippet\":{},\"doctestRole\":{},\"doctestPath\":{},\"doctestLine\":{}}}\n",
                escape(&crate_name.to_string()),
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
            );
            let _ = output.write_all(record.as_bytes());
        }
        let _ = output.flush();
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
    let mut instrumented = body.clone();
    let definition = tcx.def_path_str(def_id);
    let Some(probe_id) = probe_id_for(&definition) else {
        return body;
    };
    let Some(probe_function) = tcx
        .hir_free_items()
        .map(|item| item.owner_id.def_id)
        .find(|item| tcx.def_path_str(*item).ends_with(PROBE_FUNCTION))
    else {
        return body;
    };

    let span = tcx.def_span(def_id);
    let unit = instrumented
        .local_decls
        .push(LocalDecl::new(tcx.types.unit, span));
    let continuation = {
        let original_start =
            instrumented.basic_blocks_mut()[rustc_middle::mir::START_BLOCK].clone();
        instrumented.basic_blocks_mut().push(original_start)
    };
    instrumented.basic_blocks_mut()[rustc_middle::mir::START_BLOCK] = BasicBlockData::new(
        Some(Terminator {
            source_info: SourceInfo::outermost(span),
            kind: TerminatorKind::Call {
                func: Operand::function_handle(tcx, probe_function.to_def_id(), [], span),
                args: [Spanned {
                    node: Operand::const_from_scalar(
                        tcx,
                        tcx.types.u64,
                        Scalar::from_u64(probe_id),
                        span,
                    ),
                    span: DUMMY_SP,
                }]
                .into(),
                destination: Place::from(unit),
                target: Some(continuation),
                unwind: UnwindAction::Continue,
                call_source: CallSource::Misc,
                fn_span: span,
            },
        }),
        false,
    );
    tcx.arena.alloc(instrumented)
}

fn probe_id_for(definition: &str) -> Option<u64> {
    for (suffix, probe_id) in [
        ("authored", 0),
        ("fallible", 1),
        ("drop_order", 2),
        ("panic_path", 3),
    ] {
        if definition.ends_with(suffix) {
            return Some(probe_id);
        }
    }
    None
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
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
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
