//! Thin syntax inventories and frozen input capture for assertion maps.
//! No type/flow/dependency verifier belongs here.
use crate::{
    assertion_map::{Anchor, Files, Inputs, InventorySite, local_path},
    evidence_archive::EvidenceArchiveEntry,
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub const ARCHIVE_PATH: &str = "assertion-inputs.json";

pub fn capture(
    root: &Path,
    language: &str,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Inputs, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    // Store only a digest of environment inputs, never their values. Engine
    // run IDs, working directories and shell nesting are not semantic inputs.
    let environment = std::env::vars()
        .filter(|(k, _)| {
            !k.starts_with("SUPERCOV_") && !matches!(k.as_str(), "PWD" | "OLDPWD" | "SHLVL" | "_")
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut inputs = Inputs { schema_version: 1, language: language.into(), context_digest: crate::assertion_map::digest(&environment), files: Files::new(), assertions: vec![], limitations: vec![
        "Syntax inventory covers recognized assertion forms, not every possible custom assertion. Agents may add exact source sites; missing runtime identity never earns credit.".into()
    ] };
    for path in paths.into_iter().collect::<BTreeSet<_>>() {
        let full = root.join(&path);
        if !full.exists() {
            continue;
        }
        let relative = full
            .strip_prefix(&root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if !local_path(&relative)
            || !full
                .canonicalize()
                .map_err(|e| e.to_string())?
                .starts_with(&root)
        {
            return Err(format!("assertion input outside project: {relative}"));
        }
        let bytes = fs::read(&full).map_err(|e| format!("{relative}: {e}"))?;
        let Ok(text) = String::from_utf8(bytes) else {
            inputs.limitations.push(format!(
                "Non-UTF-8 input omitted from source anchors: {relative}"
            ));
            continue;
        };
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        let ranges = match extension {
            "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => {
                crate::js_instrumenter::assertion_ranges(&relative, &text)
            }
            "rs" => rust_ranges(&text),
            "py" => python_ranges(&text),
            "rb" => ruby_ranges(&text),
            _ => Ok(vec![]),
        };
        match ranges {
            Ok(ranges) => {
                inputs
                    .assertions
                    .extend(
                        ranges
                            .into_iter()
                            .map(|(start, end, operation)| InventorySite {
                                at: Anchor::new(&relative, &text, start, end),
                                operation,
                            }),
                    )
            }
            Err(e) => inputs
                .limitations
                .push(format!("Inventory unavailable for {relative}: {e}")),
        }
        inputs.files.insert(relative, text);
    }
    inputs.assertions.sort_by(|a, b| a.at.cmp(&b.at));
    Ok(inputs)
}

pub fn append(
    mut entries: Vec<EvidenceArchiveEntry>,
    inputs: &Inputs,
) -> Result<Vec<EvidenceArchiveEntry>, String> {
    if entries.iter().any(|e| e.path == ARCHIVE_PATH) {
        return Err("duplicate assertion inputs".into());
    }
    entries.push(EvidenceArchiveEntry {
        path: ARCHIVE_PATH.into(),
        contents: serde_json::to_vec(inputs).map_err(|e| e.to_string())?,
    });
    Ok(entries)
}

fn rust_ranges(source: &str) -> Result<Vec<(usize, usize, String)>, String> {
    use ra_ap_syntax::{AstNode, Edition, SourceFile, ast};
    let parsed = SourceFile::parse(source, Edition::Edition2024);
    if !parsed.errors().is_empty() {
        return Err("Rust parse errors".into());
    }
    Ok(parsed
        .tree()
        .syntax()
        .descendants()
        .filter_map(ast::MacroCall::cast)
        .filter_map(|m| {
            let path = m.path()?.syntax().text().to_string();
            if !matches!(
                path.rsplit("::").next()?,
                "assert"
                    | "assert_eq"
                    | "assert_ne"
                    | "debug_assert"
                    | "debug_assert_eq"
                    | "debug_assert_ne"
            ) {
                return None;
            }
            let range = m.syntax().text_range();
            Some((
                u32::from(range.start()) as usize,
                u32::from(range.end()) as usize,
                path,
            ))
        })
        .collect())
}
fn python_ranges(source: &str) -> Result<Vec<(usize, usize, String)>, String> {
    use ruff_python_ast::{
        Expr, Stmt,
        visitor::{Visitor, walk_expr, walk_stmt},
    };
    use ruff_text_size::Ranged;
    struct Collector(Vec<(usize, usize, String)>);
    impl<'a> Visitor<'a> for Collector {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            if let Stmt::Assert(_) = stmt {
                self.0.push((
                    stmt.range().start().to_usize(),
                    stmt.range().end().to_usize(),
                    "assert".into(),
                ));
            }
            walk_stmt(self, stmt);
        }
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Call(call) = expr
                && let Expr::Attribute(attr) = call.func.as_ref()
                && attr.attr.as_str().starts_with("assert")
            {
                self.0.push((
                    expr.range().start().to_usize(),
                    expr.range().end().to_usize(),
                    attr.attr.to_string(),
                ));
            }
            walk_expr(self, expr);
        }
    }
    let parsed = ruff_python_parser::parse_module(source).map_err(|e| e.to_string())?;
    let mut collector = Collector(vec![]);
    for stmt in &parsed.syntax().body {
        collector.visit_stmt(stmt);
    }
    Ok(collector.0)
}
fn ruby_ranges(source: &str) -> Result<Vec<(usize, usize, String)>, String> {
    use ruby_prism::{CallNode, Visit};
    struct Collector(Vec<(usize, usize, String)>);
    impl<'a> Visit<'a> for Collector {
        fn visit_call_node(&mut self, node: &CallNode<'a>) {
            let name = String::from_utf8_lossy(node.name().as_slice()).into_owned();
            if name == "assert"
                || name == "refute"
                || name.starts_with("assert_")
                || name.starts_with("refute_")
                || matches!(name.as_str(), "to" | "not_to" | "to_not")
            {
                let location = node.location();
                self.0
                    .push((location.start_offset(), location.end_offset(), name));
            }
            ruby_prism::visit_call_node(self, node);
        }
    }
    let parsed = ruby_prism::parse(source.as_bytes());
    if parsed.errors().next().is_some() {
        return Err("Ruby parse errors".into());
    }
    let mut collector = Collector(vec![]);
    collector.visit(&parsed.node());
    Ok(collector.0)
}
