//! Thin syntax inventories and source fingerprints for assertion maps.
//! No type/flow/dependency verifier belongs here.
use crate::{
    assertion_map::{
        Anchor, FileFingerprint, Files, InputManifest, Inputs, InventorySite, local_path,
    },
    evidence_archive::EvidenceArchiveEntry,
    workspace::{canonicalize_simplified, simplified},
};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

pub const ARCHIVE_PATH: &str = "assertion-inputs.json";

/// Names the variables whose values participate in assertion context identity,
/// as a comma-separated list. Empty or unset means none.
pub const CONTEXT_ENVIRONMENT: &str = "SUPERCOV_ASSERTION_CONTEXT_ENV";

/// Identity of the execution context an authored claim was reviewed against.
///
/// The ambient process environment is deliberately **not** part of this. A run
/// from a different directory, terminal session, package manager or Node
/// installation carries dozens of incidental variables (`INIT_CWD`, `npm_*`,
/// `TERM_SESSION_ID`, per-session sockets and tokens), and folding those in
/// invalidated every flow in a map at once for no semantic reason. Environment
/// differences that actually change behaviour are already caught where it
/// matters: build-relevant variables participate in the run's configuration,
/// dependency and instrumenter fingerprints, and any real behavioural change
/// shows up in re-collected evidence, because credit requires a passing
/// assertion occurrence and execution of the claimed statement in the same
/// selected test.
///
/// Projects that genuinely depend on specific variables name them in
/// [`CONTEXT_ENVIRONMENT`]; only those participate, and a variable that is not
/// set is recorded as absent rather than skipped.
fn context_digest() -> String {
    selected_context_digest(
        &std::env::var(CONTEXT_ENVIRONMENT).unwrap_or_default(),
        |name| std::env::var(name).ok(),
    )
}

fn selected_context_digest(names: &str, value: impl Fn(&str) -> Option<String>) -> String {
    let selected = names
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(|name| (name.to_owned(), value(name)))
        .collect::<std::collections::BTreeMap<_, _>>();
    crate::assertion_map::digest(&("supercov-assertion-context-v2", selected))
}

pub fn capture(
    root: &Path,
    language: &str,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<Inputs, String> {
    capture_with_expect_modules(root, language, paths, &[])
}

pub fn capture_with_expect_modules(
    root: &Path,
    language: &str,
    paths: impl IntoIterator<Item = PathBuf>,
    expect_modules: &[String],
) -> Result<Inputs, String> {
    let supplied_root = simplified(root.to_owned());
    let root = canonicalize_simplified(root).map_err(|e| e.to_string())?;
    // Store only a digest of the selected context, never its values.
    let mut inputs = Inputs { schema_version: 1, language: language.into(), context_digest: context_digest(), files: Files::new(), assertions: vec![], limitations: vec![
        "Syntax inventory covers recognized assertion forms, not every possible custom assertion. Agents may add exact source sites; missing runtime identity never earns credit.".into()
    ] };
    if language == "javascript" {
        inputs.limitations.push("Optional assertion calls are inventoried but currently have no injected phase. Unrecognized custom assertion wrappers and dynamically selected matchers may be absent. Use check --require-observed to detect inventoried sites without passing evidence.".into());
    }
    for path in paths.into_iter().map(simplified).collect::<BTreeSet<_>>() {
        let full = if path.is_absolute() {
            root.join(path.strip_prefix(&supplied_root).unwrap_or(&path))
        } else {
            root.join(&path)
        };
        if !full.exists() {
            continue;
        }
        let relative = full
            .strip_prefix(&root)
            .map_err(|_| format!("assertion input outside project: {}", full.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if !local_path(&relative)
            || !canonicalize_simplified(&full)
                .map_err(|e| e.to_string())?
                .starts_with(&root)
        {
            return Err(format!("assertion input outside project: {relative}"));
        }
        if inputs.files.contains_key(&relative) {
            continue;
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
                crate::js_instrumenter::assertion_ranges_with_expect_modules(
                    &relative,
                    &text,
                    expect_modules,
                )
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
        contents: serde_json::to_vec(&inputs.manifest()).map_err(|e| e.to_string())?,
    });
    Ok(entries)
}

/// Read project files only when their exact bytes still match the run manifest.
/// This is source identity checking, not semantic dependency analysis.
pub fn current_sources(root: &Path, manifest: &InputManifest) -> Result<Inputs, String> {
    let root = canonicalize_simplified(root).map_err(|e| e.to_string())?;
    let mut files = Files::new();
    for (file, expected) in &manifest.files {
        if !local_path(file) {
            return Err(format!("Invalid assertion input path: {file}"));
        }
        let path = root.join(file);
        let source = (|| {
            let canonical = canonicalize_simplified(&path).ok()?;
            if !canonical.starts_with(&root) || !canonical.is_file() {
                return None;
            }
            let text = fs::read_to_string(canonical).ok()?;
            (FileFingerprint::of(&text) == *expected).then_some(text)
        })();
        let Some(source) = source else {
            return Err(format!(
                "Current source differs from the run or is unavailable: {file}; rerun tests to inherit the map for the current checkout"
            ));
        };
        files.insert(file.clone(), source);
    }
    let inputs = manifest.with_sources(files);
    if inputs
        .assertions
        .iter()
        .any(|s| s.at.offset(&inputs.files).is_none())
    {
        return Err("Invalid assertion identities in run manifest".into());
    }
    Ok(inputs)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn empty(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn incidental_environment_never_reaches_context_identity() {
        // The values a shell, package manager or terminal happens to export are
        // not semantic inputs; without an explicit selection the identity is a
        // constant, so a map authored in one session stays current in the next.
        let baseline = selected_context_digest("", empty);
        assert_eq!(
            baseline,
            selected_context_digest("", |_| {
                panic!("no variable may be read without an explicit selection")
            })
        );
        assert_eq!(baseline, selected_context_digest("  ,  ,", empty));
    }

    #[test]
    fn explicitly_selected_variables_participate_and_distinguish_absence() {
        let unset = selected_context_digest("TZ", empty);
        let utc = selected_context_digest("TZ", |name| (name == "TZ").then(|| "UTC".to_owned()));
        let berlin = selected_context_digest("TZ", |name| {
            (name == "TZ").then(|| "Europe/Berlin".to_owned())
        });
        assert_ne!(unset, utc, "an unset variable differs from a set one");
        assert_ne!(utc, berlin, "the value participates, not just the name");
        assert_ne!(
            utc,
            selected_context_digest("", empty),
            "selecting a variable differs from selecting none"
        );
        // Order and padding in the selection are not themselves inputs.
        let pair = selected_context_digest("TZ,LANG", |name| Some(name.to_owned()));
        assert_eq!(
            pair,
            selected_context_digest(" LANG , TZ ", |name| Some(name.to_owned()))
        );
    }
}
