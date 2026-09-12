//! Opt-in, compile-time-only identity evidence. No MIR/runtime changes.
use super::*;
use rustc_hir::def::Res;

pub(super) fn enabled(tcx: TyCtxt<'_>) -> bool {
    // Cargo and run integrity already track rustc cfg flags. An untracked
    // environment toggle could silently reuse candidates without this data.
    tcx.sess.psess.config.contains(&(
        rustc_span::Symbol::intern("supercov_assertion_identities"),
        None,
    ))
}

pub(super) fn collect<'tcx>(
    tcx: TyCtxt<'tcx>,
    owner: LocalDefId,
    owner_point: &str,
    crate_name: &str,
    records: &mut BTreeMap<String, serde_json::Value>,
) {
    struct Collector<'a, 'tcx> {
        tcx: TyCtxt<'tcx>,
        owner: LocalDefId,
        owner_point: &'a str,
        crate_name: &'a str,
        records: &'a mut BTreeMap<String, serde_json::Value>,
    }
    impl Collector<'_, '_> {
        fn record(&mut self, kind: &str, span: rustc_span::Span, target: String, standard: bool) {
            let Ok(source) = stable_source_range(self.tcx, span, self.crate_name) else {
                return;
            };
            if !source.owned || !source.key.starts_with("source:") {
                return;
            }
            let record = serde_json::json!({"kind":kind, "sourceKey":source.key,
                "start":source.start, "end":source.end, "owner":self.owner_point,
                "target":target, "standardEquality":standard});
            // Preserve contradictory records; consumers must not pick one.
            self.records.insert(record.to_string(), record);
        }
    }
    impl<'tcx> Visitor<'tcx> for Collector<'_, 'tcx> {
        fn visit_expr(&mut self, expression: &'tcx hir::Expr<'tcx>) {
            if let hir::ExprKind::Call(callee, _) = expression.kind
                && let hir::ExprKind::Path(ref path) = callee.kind
                && let Res::Def(DefKind::Fn, target) =
                    self.tcx.typeck(self.owner).qpath_res(path, callee.hir_id)
                && !expression.span.from_expansion()
                && let Ok(identity) =
                    function_identity(self.tcx, target, self.tcx.def_span(target), self.crate_name)
            {
                self.record("call", expression.span, identity.id, false);
            }
            let mut span = expression.span;
            for _ in 0..1024 {
                if span.ctxt().is_root() {
                    break;
                }
                let frame = span.ctxt().outer_expn_data();
                if let Some(target) = frame.macro_def_id {
                    let standard = self
                        .tcx
                        .get_diagnostic_item(rustc_span::Symbol::intern("assert_eq_macro"))
                        == Some(target);
                    self.record(
                        "macro",
                        frame.call_site,
                        exact_def_path!(self.tcx, target),
                        standard,
                    );
                }
                span = frame.call_site;
            }
            intravisit::walk_expr(self, expression);
        }
    }
    Collector {
        tcx,
        owner,
        owner_point,
        crate_name,
        records,
    }
    .visit_body(tcx.hir_body_owned_by(owner));
}
