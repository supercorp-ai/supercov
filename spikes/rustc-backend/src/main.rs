#![feature(rustc_private)]

extern crate rustc_ast;
extern crate rustc_driver;
extern crate rustc_errors;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
extern crate rustc_parse;
extern crate rustc_session;
extern crate rustc_span;

use std::{
    env,
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
    sync::OnceLock,
};

use rustc_driver::{Callbacks, Compilation};
use rustc_errors::ErrorGuaranteed;
use rustc_hir::def::DefKind;
use rustc_interface::interface::{Compiler, Config};
use rustc_middle::{
    mir::{
        BasicBlockData, Body, CallSource, LocalDecl, Operand, Place, SourceInfo, Terminator,
        TerminatorKind, UnwindAction, interpret::Scalar,
    },
    ty::TyCtxt,
    util::Providers,
};
use rustc_session::Session;
use rustc_span::{DUMMY_SP, FileName, def_id::LocalDefId, source_map::Spanned};

use rustc_parse::{lexer::StripTokens, new_parser_from_source_str};

const OUTPUT_DIRECTORY: &str = "SUPERCOV_RUSTC_SPIKE_OUTPUT";
const INSTRUMENT_MIR: &str = "SUPERCOV_RUSTC_SPIKE_INSTRUMENT_MIR";
const PROBE_FUNCTION: &str = "__supercov_spike_runtime::probe";
const INJECTED_RUNTIME: &str = r#"
#[doc(hidden)]
#[allow(dead_code)]
mod __supercov_spike_runtime {
    use core::sync::atomic::{AtomicU64, Ordering};

    static PROBE_MASK: AtomicU64 = AtomicU64::new(0);

    #[inline(never)]
    pub(crate) fn probe(probe_id: u64) {
        PROBE_MASK.fetch_or(1_u64 << probe_id, Ordering::Relaxed);
    }

    pub(crate) fn probe_mask() -> u64 {
        PROBE_MASK.load(Ordering::Relaxed)
    }
}
"#;

type OptimizedMirProvider = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Body<'tcx>;

static ORIGINAL_OPTIMIZED_MIR: OnceLock<OptimizedMirProvider> = OnceLock::new();

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
        let mut parser = rustc_parse::unwrap_or_emit_fatal(new_parser_from_source_str(
            &compiler.sess.psess,
            FileName::Custom("<supercov-rust-runtime>".into()),
            INJECTED_RUNTIME.into(),
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
            let record = format!(
                "{{\"crate\":\"{}\",\"definition\":\"{}\",\"kind\":\"{:?}\",\"span\":\"{}\",\"callsite\":\"{}\",\"expanded\":{},\"mir_blocks\":{}}}\n",
                escape(&crate_name.to_string()),
                escape(&tcx.def_path_str(def_id)),
                kind,
                escape(&source_map.span_to_diagnostic_string(span)),
                escape(&source_map.span_to_diagnostic_string(callsite)),
                span.from_expansion(),
                mir.basic_blocks.len(),
            );
            let _ = output.write_all(record.as_bytes());
        }
        let _ = output.flush();
        Compilation::Continue
    }
}

fn install_query_overrides(_session: &Session, providers: &mut Providers) {
    let _ = ORIGINAL_OPTIMIZED_MIR.set(providers.queries.optimized_mir);
    providers.queries.optimized_mir = optimized_mir_with_probe;
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

fn main() {
    let mut args = env::args().skip(1).collect::<Vec<_>>();
    if env::var_os(INSTRUMENT_MIR).is_some() {
        args.push("--cfg=supercov_spike_instrumented".into());
        args.push("--check-cfg=cfg(supercov_spike_instrumented)".into());
    }
    let mut callbacks = ProbeCallbacks;
    rustc_driver::run_compiler(&args, &mut callbacks);
}
