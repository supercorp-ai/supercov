#![feature(rustc_private)]

extern crate rustc_driver;
extern crate rustc_hir;
extern crate rustc_interface;
extern crate rustc_middle;
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
use rustc_hir::def::DefKind;
use rustc_interface::interface::{Compiler, Config};
use rustc_middle::{
    mir::{Body, SwitchTargets, TerminatorKind},
    ty::TyCtxt,
    util::Providers,
};
use rustc_session::Session;
use rustc_span::def_id::LocalDefId;

const OUTPUT_DIRECTORY: &str = "SUPERCOV_RUSTC_SPIKE_OUTPUT";
const MUTATE_MIR: &str = "SUPERCOV_RUSTC_SPIKE_MUTATE_MIR";

type OptimizedMirProvider = for<'tcx> fn(TyCtxt<'tcx>, LocalDefId) -> &'tcx Body<'tcx>;

static ORIGINAL_OPTIMIZED_MIR: OnceLock<OptimizedMirProvider> = OnceLock::new();

struct ProbeCallbacks;

impl Callbacks for ProbeCallbacks {
    fn config(&mut self, config: &mut Config) {
        config.override_queries = Some(install_query_overrides);
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
    if env::var_os(MUTATE_MIR).is_none() {
        return body;
    }
    let mut instrumented = body.clone();
    if tcx.def_path_str(def_id).ends_with("authored") {
        for block in instrumented.basic_blocks_mut() {
            let Some(terminator) = &mut block.terminator else {
                continue;
            };
            let TerminatorKind::SwitchInt { targets, .. } = &mut terminator.kind else {
                continue;
            };
            if let Some((value, then_target, else_target)) = targets.as_static_if() {
                *targets = SwitchTargets::static_if(value, else_target, then_target);
                break;
            }
        }
    }
    tcx.arena.alloc(instrumented)
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
    let args = env::args().skip(1).collect::<Vec<_>>();
    let mut callbacks = ProbeCallbacks;
    rustc_driver::run_compiler(&args, &mut callbacks);
}
