//! A deliberately small, source-grounded recognizer; not a general Rust resolver.
//!
//! Recognizes primitive integer/bool results passed unchanged to standard
//! assert_eq!, directly or via immutable local copies. A value witness is conditional
//! on this invocation returning normally: it is NOT a prediction that every
//! mutation inside the function changes that value or is killed.

use std::collections::{BTreeMap, BTreeSet};

use ra_ap_syntax::{
    AstNode, Edition, SourceFile, SyntaxNode,
    ast::{self, HasArgList, HasAttrs, HasGenericParams, HasModuleItem, HasName},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::{
    asserted_coverage::{Boundary, Facts, Observation, Strength},
    coverage_analysis::PointKind,
    coverage_report::{CoverageManifest, RawTestResult},
    run_store::RunFingerprint,
    rust_asserted_coverage::analyze_runtime_assertions,
    rust_compiler_manifest::{ResolvedRustAssertionIdentities, ResolvedRustAssertionIdentity},
};

pub struct RustAssertionSources {
    /// Original, complete Rust source bytes, keyed by run-relative path.
    pub files: BTreeMap<String, String>,
    /// These two identities must come from the run-matching Cargo manifest.
    pub crate_name: String,
    pub library_file: String,
}

impl RustAssertionSources {
    /// Match the exact source/test domains of the existing run fingerprint.
    /// Do not accept only matching assertion snippets: aliases elsewhere can
    /// change their meaning. This hashes the bytes actually being analyzed.
    pub fn verify(&self, fingerprint: &RunFingerprint) -> Result<(), String> {
        let mut hash = Sha256::new();
        for (path, source) in &self.files {
            if path.is_empty()
                || path
                    .split('/')
                    .any(|s| s.is_empty() || matches!(s, "." | ".."))
                || path.contains('\\')
            {
                return Err("noncanonical source snapshot path".into());
            }
            hash.update(path.as_bytes());
            hash.update([0]);
            hash.update(source.as_bytes());
            hash.update([0]);
        }
        let digest = format!("{:x}", hash.finalize());
        if fingerprint.algorithm != "sha256"
            || fingerprint.source != digest
            || fingerprint.tests != digest
            || fingerprint.source_files != self.files.len()
            || fingerprint.test_files != self.files.len()
        {
            return Err(
                "source snapshot does not match the run's complete Rust source/test fingerprint"
                    .into(),
            );
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExactValueWitness {
    pub point: String,
    pub attempt: String,
    pub phase: String,
    pub assertion: String,
    pub producer: String,
    pub call: String,
    pub expected: String,
    pub primitive: String,
    pub via_local: bool,
    /// Source-ordered binding versions used by this value, not every local in
    /// the test. Empty for a direct assertion operand.
    pub bindings: Vec<ImmutableValueBinding>,
    /// Both callee and equality macro were resolved by the compiler.
    pub compiler_resolved: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImmutableValueBinding {
    pub name: String,
    pub at: String,
    pub initializer: String,
}

struct BindingVersion {
    value: ImmutableValueBinding,
    parent: Option<usize>,
}

fn binding_path(
    mut version: Option<usize>,
    versions: &[BindingVersion],
) -> Vec<ImmutableValueBinding> {
    let mut path = Vec::new();
    while let Some(index) = version {
        let binding = &versions[index];
        path.push(binding.value.clone());
        version = binding.parent;
    }
    path.reverse();
    path
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RustSourceAssertionAnalysis {
    pub facts: Facts,
    pub witnesses: Vec<ExactValueWitness>,
    pub limits: Vec<String>,
}

#[derive(Clone)]
struct Producer {
    point: String,
    primitive: String,
    call: String,
    location: String,
    binding: Option<usize>,
}

fn compact(node: &SyntaxNode) -> String {
    node.descendants_with_tokens()
        .filter_map(|e| e.into_token())
        .filter(|t| !t.kind().is_trivia())
        .map(|t| t.text().to_string())
        .collect()
}

fn primitive(name: &str) -> bool {
    matches!(
        name,
        "bool"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
    )
}

fn location(file: &str, source: &str, node: &SyntaxNode) -> String {
    let offset = usize::from(node.text_range().start());
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|b| *b == b'\n').count() + 1;
    let column = offset - prefix.rfind('\n').map_or(0, |p| p + 1);
    format!("{file}:{line}:{column}") // Rust compiler columns are zero-based UTF-8 bytes.
}

fn parse(source: &str) -> Result<SourceFile, String> {
    let parsed = SourceFile::parse(source, Edition::Edition2024);
    if !parsed.errors().is_empty() {
        return Err(format!("Rust source parse errors: {:?}", parsed.errors()));
    }
    Ok(parsed.tree())
}

fn literal(expr: &ast::Expr) -> bool {
    match expr {
        ast::Expr::Literal(value) => matches!(value.kind(), ast::LiteralKind::IntNumber(_) | ast::LiteralKind::Bool(_)),
        ast::Expr::PrefixExpr(value) => value.op_kind() == Some(ast::UnaryOp::Neg)
            && value.expr().is_some_and(|e| matches!(e, ast::Expr::Literal(ref v) if matches!(v.kind(), ast::LiteralKind::IntNumber(_)))),
        _ => false,
    }
}

fn equality_arguments(statement: &ast::Stmt) -> Option<(ast::MacroCall, Vec<ast::Expr>)> {
    if statement
        .syntax()
        .descendants()
        .any(|n| ast::Attr::can_cast(n.kind()))
    {
        return None;
    }
    let call = match statement {
        ast::Stmt::Item(ast::Item::MacroCall(call)) => call.clone(),
        ast::Stmt::ExprStmt(statement) => {
            let ast::Expr::MacroExpr(expr) = statement.expr()? else {
                return None;
            };
            expr.macro_call()?
        }
        _ => return None,
    };
    if call.attrs().next().is_some() {
        return None;
    }
    if !matches!(
        compact(call.path()?.syntax()).as_str(),
        "assert_eq" | "std::assert_eq" | "::std::assert_eq"
    ) {
        return None;
    }
    let tree = call.token_tree()?.syntax().text().to_string();
    let inner = tree.get(1..tree.len().checked_sub(1)?)?;
    let parsed = parse(&format!("fn __args() {{ __call({inner}); }}")).ok()?;
    let expr = parsed
        .syntax()
        .descendants()
        .find_map(ast::CallExpr::cast)?;
    let args = expr.arg_list()?.args().collect::<Vec<_>>();
    (args.len() == 2).then_some((call, args))
}

fn test_macro_namespace_is_closed(file: &SourceFile) -> bool {
    // A macro_export inside a different helper can change root path-based
    // macro lookup. Include/other expansions can hide such definitions, even
    // inside an assertion's opaque token tree. Reject those across the whole
    // test file, not just in the function supplying this witness.
    file.syntax().descendants().all(|node| {
        if ast::MacroRules::can_cast(node.kind()) || ast::MacroDef::can_cast(node.kind()) {
            return false;
        }
        let Some(call) = ast::MacroCall::cast(node) else {
            return true;
        };
        let Some(path) = call.path().map(|p| compact(p.syntax())) else {
            return false;
        };
        let name = path
            .strip_prefix("::std::")
            .or_else(|| path.strip_prefix("std::"))
            .unwrap_or(&path);
        matches!(
            name,
            "assert"
                | "assert_eq"
                | "assert_ne"
                | "debug_assert"
                | "debug_assert_eq"
                | "debug_assert_ne"
        ) && call.token_tree().is_some_and(|tree| {
            !tree
                .syntax()
                .descendants_with_tokens()
                .filter_map(|n| n.into_token())
                .any(|token| matches!(token.text(), "!" | "macro"))
        })
    })
}

fn imports(
    tree: ast::UseTree,
    prefix: &str,
    crate_name: &str,
    names: &mut BTreeSet<String>,
    allow_renames: bool,
) -> bool {
    if tree.rename().is_some() && !allow_renames {
        return false;
    }
    let part = tree.path().map(|p| compact(p.syntax())).unwrap_or_default();
    let path = if prefix.is_empty() {
        part
    } else if part.is_empty() {
        prefix.into()
    } else {
        format!("{prefix}::{part}")
    };
    if let Some(list) = tree.use_tree_list() {
        return list
            .use_trees()
            .all(|child| imports(child, &path, crate_name, names, allow_renames));
    }
    if tree.star_token().is_some() && path == crate_name {
        names.insert("*".into());
        return true;
    }
    if let Some(name) = path.strip_prefix(&format!("{crate_name}::"))
        && !name.contains("::")
        && !name.is_empty()
    {
        if let Some(rename) = tree.rename() {
            let Some(alias) = rename.name() else {
                return false;
            };
            names.insert(alias.text().to_string());
        } else {
            names.insert(name.into());
        }
        return true;
    }
    false
}

fn compiler_identities(
    manifest: &CoverageManifest,
    sources: &RustAssertionSources,
) -> Result<Option<ResolvedRustAssertionIdentities>, String> {
    let Some(value) = manifest
        .scope
        .as_ref()
        .and_then(|s| s.get("assertionIdentities"))
    else {
        return Ok(None);
    };
    let identities: ResolvedRustAssertionIdentities = serde_json::from_value(value.clone())
        .map_err(|e| format!("invalid compiler assertion identities: {e}"))?;
    identities.validate(manifest).map_err(|e| e.to_string())?;
    for record in &identities.records {
        if !matches!(record.kind.as_str(), "call" | "macro")
            || record.target.is_empty()
            || record.kind == "call" && record.standard_equality
            || record.start >= record.end
            || sources
                .files
                .get(&record.file)
                .and_then(|s| s.get(record.start as usize..record.end as usize))
                != Some(record.source.as_str())
            || !manifest.points.iter().any(|p| {
                p.id == record.owner && p.kind == PointKind::Function && p.file == record.file
            })
        {
            return Err("compiler assertion identity does not match source/owner".into());
        }
    }
    Ok(Some(identities))
}

fn unique_identity<'a>(
    identities: &'a ResolvedRustAssertionIdentities,
    kind: &str,
    file: &str,
    region: &SyntaxNode,
    text: &str,
) -> Option<&'a ResolvedRustAssertionIdentity> {
    let range = region.text_range();
    let mut matches = identities.records.iter().filter(|r| {
        r.kind == kind
            && r.file == file
            && r.start >= u32::from(range.start())
            && r.end <= u32::from(range.end())
            && r.source == text
    });
    let identity = matches.next()?;
    matches.next().is_none().then_some(identity)
}

struct CallResolver<'a> {
    catalog: &'a BTreeMap<String, (String, String)>,
    imported: &'a BTreeSet<String>,
    test_names: &'a BTreeSet<String>,
    crate_name: &'a str,
    file: &'a str,
    identities: Option<&'a ResolvedRustAssertionIdentities>,
}

impl CallResolver<'_> {
    fn resolve(
        &self,
        expr: &ast::Expr,
        locals: &BTreeMap<String, Producer>,
        region: &SyntaxNode,
        location: &str,
    ) -> Option<Producer> {
        let ast::Expr::CallExpr(call) = expr else {
            return None;
        };
        let ast::Expr::PathExpr(callee) = call.expr()? else {
            return None;
        };
        if !call.arg_list()?.args().all(|e| literal(&e)) {
            return None;
        }
        let text = expr.syntax().text().to_string();
        let (point, primitive) = if let Some(identities) = self.identities {
            // The restricted grammar admits exactly one direct call with literal
            // arguments in this statement/operand. Matching full source bytes in
            // that region avoids inventing offsets for reparsed macro arguments.
            // No spelling fallback if compiler data is missing or ambiguous.
            let binding = unique_identity(identities, "call", self.file, region, &text)?;
            self.catalog
                .values()
                .find(|(point, _)| point == &binding.target)?
        } else {
            let path = compact(callee.syntax());
            let name = if let Some(name) = path.strip_prefix(&format!("{}::", self.crate_name)) {
                if locals.contains_key(self.crate_name) || self.test_names.contains(self.crate_name)
                {
                    return None;
                }
                name
            } else {
                if locals.contains_key(&path)
                    || self.test_names.contains(&path)
                    || !(self.imported.contains("*") || self.imported.contains(&path))
                {
                    return None;
                }
                path.as_str()
            };
            self.catalog.get(name)?
        };
        Some(Producer {
            point: point.clone(),
            primitive: primitive.clone(),
            call: text,
            location: location.into(),
            binding: None,
        })
    }
}

/// The source fingerprint is required, not an optional confidence flag. The
/// caller also authenticates Cargo identities and source-to-run provenance.
pub fn analyze_source_assertions(
    manifest: &CoverageManifest,
    results: &[RawTestResult],
    source_files: &BTreeSet<String>,
    sources: &RustAssertionSources,
    fingerprint: &RunFingerprint,
) -> Result<RustSourceAssertionAnalysis, String> {
    sources.verify(fingerprint)?;
    let identities = compiler_identities(manifest, sources)?;
    let baseline = analyze_runtime_assertions(manifest, results, source_files)?;
    let mut output = RustSourceAssertionAnalysis {
        facts: baseline.facts,
        witnesses: Vec::new(),
        limits: Vec::new(),
    };
    let Some(library_text) = sources.files.get(&sources.library_file) else {
        return Err("missing library source".into());
    };
    let library = parse(library_text)?;
    // No imports, aliases, modules, expansions or attributes may change bare
    // primitive names or export a shadow assertion macro. Structs/impls are
    // allowed only so custom equality remains a visible unsupported result.
    if library.attrs().next().is_some()
        || library.syntax().descendants().any(|n| {
            ast::MacroCall::can_cast(n.kind())
                || ast::MacroRules::can_cast(n.kind())
                || ast::MacroDef::can_cast(n.kind())
        })
        || library.items().any(|item| match item {
            ast::Item::Fn(ref f) => f.attrs().next().is_some(),
            ast::Item::Struct(ref s) => {
                s.attrs().next().is_some()
                    || s.name()
                        .is_none_or(|n| primitive(n.text()) || n.text() == "std")
            }
            ast::Item::Impl(ref i) => i.attrs().next().is_some(),
            _ => true,
        })
    {
        output.limits.push("unsupported library namespace: imports, aliases, modules, macros or attributes need compiler-resolved identities".into());
        return Ok(output);
    }
    let mut catalog = BTreeMap::new();
    for function in library.items().filter_map(|i| {
        if let ast::Item::Fn(f) = i {
            Some(f)
        } else {
            None
        }
    }) {
        if function.async_token().is_some()
            || function.unsafe_token().is_some()
            || function.abi().is_some()
            || function.generic_param_list().is_some()
            || function.const_token().is_some()
        {
            continue;
        }
        let Some(name) = function.name().map(|n| n.text().to_string()) else {
            continue;
        };
        let Some(ty) = function
            .ret_type()
            .and_then(|t| t.ty())
            .map(|t| compact(t.syntax()))
        else {
            continue;
        };
        if !primitive(&ty) {
            continue;
        }
        let at = location(&sources.library_file, library_text, function.syntax());
        let candidates = manifest
            .points
            .iter()
            .filter(|p| {
                p.kind == PointKind::Function
                    && format!("{}:{}:{}", p.file, p.line, p.column) == at
                    && function.syntax().text().to_string().starts_with(&p.source)
                    && !manifest.unmeasured.contains(&p.id)
            })
            .collect::<Vec<_>>();
        if let [point] = candidates.as_slice()
            && catalog.insert(name, (point.id.clone(), ty)).is_some()
        {
            return Err("ambiguous function identity".into());
        }
    }
    for (index, result) in results.iter().enumerate() {
        let attempt = format!(
            "rust-attempt:{index}:{}",
            result.test_id.as_deref().unwrap_or(&result.test)
        );
        let Some(test_index) = output.facts.tests.iter().position(|t| t.id == attempt) else {
            continue;
        };
        let Some(file) = result.test_file.as_ref() else {
            continue;
        };
        let Some(text) = sources.files.get(file) else {
            output.limits.push(format!("missing test source: {file}"));
            continue;
        };
        let parsed = parse(text)?;
        let mut imported = BTreeSet::new();
        let mut test_names = BTreeSet::new();
        let mut functions = Vec::new();
        let mut supported =
            parsed.attrs().next().is_none() && test_macro_namespace_is_closed(&parsed);
        for item in parsed.items() {
            match item {
                ast::Item::Use(u) => {
                    supported &= u.attrs().next().is_none()
                        && u.use_tree().is_some_and(|t| {
                            imports(
                                t,
                                "",
                                &sources.crate_name,
                                &mut imported,
                                identities.is_some(),
                            )
                        })
                }
                ast::Item::Fn(f) => {
                    if let Some(name) = f.name() {
                        test_names.insert(name.text().to_string());
                    }
                    functions.push(f);
                }
                _ => supported = false,
            }
        }
        if !supported {
            output
                .limits
                .push(format!("unsupported test namespace: {file}"));
            continue;
        }
        let resolver = CallResolver {
            catalog: &catalog,
            imported: &imported,
            test_names: &test_names,
            crate_name: &sources.crate_name,
            file,
            identities: identities.as_ref(),
        };
        for function in functions {
            let Some(body) = function.body() else {
                continue;
            };
            // Nested items are hoisted; looking only at statements before the
            // assertion misses a later local function/import/macro shadow.
            if body
                .syntax()
                .descendants()
                .filter_map(ast::Item::cast)
                .any(|item| !matches!(item, ast::Item::MacroCall(_)))
                || function.attrs().count() != 1
                || function
                    .attrs()
                    .next()
                    .is_none_or(|a| compact(a.syntax()) != "#[test]")
                || function
                    .param_list()
                    .is_none_or(|p| p.params().next().is_some() || p.self_param().is_some())
                || function.async_token().is_some()
                || function.generic_param_list().is_some()
            {
                continue;
            }
            let Some(list) = body.stmt_list() else {
                continue;
            };
            let mut locals = BTreeMap::new();
            // Append-only versions preserve Copy semantics through shadowing.
            // An arena avoids cloning growing paths for every alias and avoids
            // recursively dropping a long self-shadowing chain.
            let mut bindings = Vec::new();
            for statement in list.statements() {
                let at = location(file, text, statement.syntax());
                if let ast::Stmt::LetStmt(binding) = &statement {
                    let Some(ast::Pat::IdentPat(pattern)) = binding.pat() else {
                        break;
                    };
                    if binding.attrs().next().is_some()
                        || binding.ty().is_some()
                        || binding.let_else().is_some()
                        || pattern.mut_token().is_some()
                        || pattern.ref_token().is_some()
                        || pattern.at_token().is_some()
                    {
                        break;
                    }
                    let Some(name) = pattern.name().map(|n| n.text().to_string()) else {
                        break;
                    };
                    let Some(initializer) = binding.initializer() else {
                        break;
                    };
                    let producer = match &initializer {
                        ast::Expr::PathExpr(path) => locals.get(&compact(path.syntax())).cloned(),
                        _ => resolver.resolve(&initializer, &locals, statement.syntax(), &at),
                    };
                    let Some(mut producer) = producer else {
                        // Do not skip an unknown initializer and leave an old
                        // same-named value live. No subsequent claim is made.
                        break;
                    };
                    bindings.push(BindingVersion {
                        value: ImmutableValueBinding {
                            name: name.clone(),
                            at,
                            initializer: initializer.syntax().text().to_string(),
                        },
                        parent: producer.binding,
                    });
                    producer.binding = Some(bindings.len() - 1);
                    locals.insert(name, producer);
                    continue;
                }
                let Some((assertion, args)) = equality_arguments(&statement) else {
                    break;
                };
                let assertion_at = location(file, text, assertion.syntax());
                let assertion_text = assertion.syntax().text().to_string();
                if let Some(identities) = &identities
                    && unique_identity(
                        identities,
                        "macro",
                        file,
                        assertion.syntax(),
                        &assertion_text,
                    )
                    .is_none_or(|r| !r.standard_equality)
                {
                    continue;
                }
                let phase = result.phases.iter().find(|p| {
                    p.kind == "assertion"
                        && p.status.as_deref() == Some("passed")
                        && p.operation == format!("Rust assertion at {assertion_at}")
                        && p.source.as_deref() == Some(assertion_text.as_str())
                });
                let Some(phase) = phase else { continue }; // RUST-ASSERT-001 stays unresolved.
                let decision = manifest.decisions.iter().find(|d| {
                    d.kind == "assertion"
                        && format!("{}:{}:{}", d.file, d.line, d.column) == assertion_at
                        && d.source == assertion_text
                });
                let Some(decision) = decision else { continue };
                if manifest.unmeasured.contains(&decision.id)
                    || !result.runtime.iter().flat_map(|s| &s.events).any(|e| {
                        e.environment == "rust"
                            && e.event_type == "decision"
                            && e.id == decision.id
                            && e.phase_id.as_deref() == Some(&phase.id)
                            && e.vector.as_ref().is_some_and(|v| v.outcome)
                    })
                {
                    continue;
                }
                let (operand, expected) = if literal(&args[1]) {
                    (&args[0], &args[1])
                } else if literal(&args[0]) {
                    (&args[1], &args[0])
                } else {
                    continue;
                };
                let via_local = matches!(operand, ast::Expr::PathExpr(_));
                let producer = if via_local {
                    locals.get(&compact(operand.syntax())).cloned()
                } else {
                    resolver.resolve(operand, &locals, assertion.syntax(), &at)
                };
                let Some(producer) = producer else { continue };
                let Some(site) = output
                    .facts
                    .sites
                    .iter_mut()
                    .find(|s| s.id == producer.point && s.covered_by.contains(&attempt))
                else {
                    continue;
                };
                // For direct calls require actual production entry in this
                // phase. Local producers use the restricted straight-line
                // grammar plus this exact attempt's production hit.
                if !via_local
                    && !baseline.execution_links.iter().any(|l| {
                        l.point == producer.point && l.attempt == attempt && l.phase == phase.id
                    })
                {
                    continue;
                }
                let boundary = format!("return:{}", producer.point);
                site.bounds = vec![Boundary {
                    boundary: boundary.clone(),
                    facet: None,
                    via: None,
                }];
                site.direct_bounds = site.bounds.clone();
                site.category = "function-result".into();
                site.unmodelled_shapes = vec![
                    "Exact return-value observation only; internal branches, other inputs and side effects are not established by this witness".into(),
                ];
                output.facts.tests[test_index]
                    .observations
                    .push(Observation {
                        boundary,
                        facet: None,
                        strength: Strength::Value,
                        where_: Some(assertion_at.clone()),
                        assertion_source: None,
                        assertion_method: None,
                        negative: false,
                        call_list: false,
                        weak: false,
                        log_sites: None,
                        pattern_shared: false,
                    });
                output.witnesses.push(ExactValueWitness {
                    compiler_resolved: identities.is_some(),
                    bindings: binding_path(producer.binding, &bindings),
                    point: producer.point,
                    attempt: attempt.clone(),
                    phase: phase.id.clone(),
                    assertion: assertion_at,
                    producer: producer.location,
                    call: producer.call,
                    expected: expected.syntax().text().to_string(),
                    primitive: producer.primitive,
                    via_local,
                });
            }
        }
    }
    output.limits.push("Only integer/bool equality, standard assertion macros, flat library identities, and straight-line immutable producer bindings/copies are recognized; all other shapes remain unknown".into());
    output.limits.push("An exact-value witness is conditional on normal return to this assertion. It does not establish that a function mutation changes the observed value, that the expected value is correct, or that all code/effects are protected".into());
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::asserted_coverage::{Status, join};
    use serde_json::json;

    // Synthetic compiler evidence for parser/guard tests only. Real compiler
    // and Cargo/mutation calibration is a separate gate.
    fn fixture(
        library: &str,
        body: &str,
    ) -> (
        CoverageManifest,
        Vec<RawTestResult>,
        RustAssertionSources,
        RunFingerprint,
    ) {
        fixture_with_import(library, body, "use sample::*;")
    }

    fn fixture_with_import(
        library: &str,
        body: &str,
        import: &str,
    ) -> (
        CoverageManifest,
        Vec<RawTestResult>,
        RustAssertionSources,
        RunFingerprint,
    ) {
        let text = format!("{import}\n#[test]\nfn test() {{\n{body}\n}}\n");
        let sources = RustAssertionSources {
            crate_name: "sample".into(),
            library_file: "src/lib.rs".into(),
            files: BTreeMap::from([
                ("src/lib.rs".into(), library.into()),
                ("tests/t.rs".into(), text.clone()),
            ]),
        };
        let mut hash = Sha256::new();
        for (path, source) in &sources.files {
            hash.update(path);
            hash.update([0]);
            hash.update(source);
            hash.update([0]);
        }
        let digest = format!("{:x}", hash.finalize());
        let fingerprint = serde_json::from_value(json!({"algorithm":"sha256", "source":digest, "tests":digest,
            "dependencies":"", "configuration":"", "instrumenter":"", "execution":"", "combined":"", "sourceFiles":2,"testFiles":2})).unwrap();
        let lib = parse(library).unwrap();
        let function = lib
            .items()
            .find_map(|i| {
                if let ast::Item::Fn(f) = i {
                    Some(f)
                } else {
                    None
                }
            })
            .unwrap();
        let at = location("src/lib.rs", library, function.syntax());
        let mut loc = at.rsplit(':');
        let column: usize = loc.next().unwrap().parse().unwrap();
        let line: usize = loc.next().unwrap().parse().unwrap();
        let signature = function
            .syntax()
            .text()
            .to_string()
            .split('{')
            .next()
            .unwrap()
            .trim()
            .to_owned();
        let mut decisions = Vec::new();
        let mut phases = Vec::new();
        let mut events = Vec::new();
        for (i, call) in parse(&text)
            .unwrap()
            .syntax()
            .descendants()
            .filter_map(ast::MacroCall::cast)
            .enumerate()
        {
            let at = location("tests/t.rs", &text, call.syntax());
            let mut loc = at.rsplit(':');
            let column: usize = loc.next().unwrap().parse().unwrap();
            let line: usize = loc.next().unwrap().parse().unwrap();
            let source = call.syntax().text().to_string();
            let id = format!("d{i}");
            let phase = format!("p{i}");
            decisions.push(json!({"id":id, "file":"tests/t.rs", "line":line,"column":column,"source":source,"conditions":[source],"kind":"assertion"}));
            phases.push(json!({"id":phase,"kind":"assertion","operation":format!("Rust assertion at {at}"),"source":source,"status":"passed","startedAtMs":0}));
            events.push(
                json!({"type":"hit","id":"f","timestampMs":0,"phaseId":phase,"environment":"rust"}),
            );
            events.push(json!({"type":"decision","id":id,"timestampMs":0,"phaseId":phase,"environment":"rust","vector":{"values":[true],"outcome":true}}));
        }
        let manifest = serde_json::from_value(json!({"decisions":decisions,"branches":[],"points":[
            {"id":"f","kind":"function","file":"src/lib.rs","line":line,"column":column,"source":signature}
        ]})).unwrap();
        let results = vec![serde_json::from_value(json!({"test":"test","testFile":"tests/t.rs","title":"test","status":"passed","phases":phases,"runtime":[{"hits":["f"],"events":events}]})).unwrap()];
        (manifest, results, sources, fingerprint)
    }

    const LIB: &str = "pub fn f(x: i32) -> i32 { x * 2 }";

    const REAL: &str = "rs:function:000000000000000000000001";
    const DECOY: &str = "rs:function:000000000000000000000002";
    const OWNER: &str = "rs:function:000000000000000000000003";

    fn identity_fixture(
        body: &str,
    ) -> (
        CoverageManifest,
        Vec<RawTestResult>,
        RustAssertionSources,
        RunFingerprint,
    ) {
        let (mut manifest, mut results, sources, fingerprint) = fixture_with_import(
            "pub fn real(x: i32) -> i32 { compute(x); x * 2 }\npub fn compute(x: i32) -> i32 { x * 2 }",
            body,
            "use sample::real as compute;",
        );
        manifest.points[0].id = REAL.into();
        manifest.points.push(
            serde_json::from_value(json!({"id":DECOY,"kind":"function",
            "file":"src/lib.rs","line":2,"column":0,"source":"pub fn compute(x: i32) -> i32"}))
            .unwrap(),
        );
        manifest.points.push(
            serde_json::from_value(json!({"id":OWNER,"kind":"function",
            "file":"tests/t.rs","line":3,"column":0,"source":"fn test()"}))
            .unwrap(),
        );
        // Both are recorded in the same assertion phase; only identity can
        // distinguish the value producer from the same-spelled callee.
        for runtime in &mut results[0].runtime {
            runtime.hits = vec![REAL.into(), DECOY.into()];
            let mut decoys = Vec::new();
            for event in &mut runtime.events {
                if event.id == "f" {
                    event.id = REAL.into();
                    let mut decoy = event.clone();
                    decoy.id = DECOY.into();
                    decoys.push(decoy);
                }
            }
            runtime.events.extend(decoys);
        }
        let text = &sources.files["tests/t.rs"];
        let record = |kind: &str, snippet: &str, target: &str, standard: bool| {
            let start = text.find(snippet).unwrap();
            json!({"kind":kind,"file":"tests/t.rs","start":start,"end":start+snippet.len(),
                "source":snippet,"owner":OWNER,"target":target,"standardEquality":standard})
        };
        let assertion = parse(text)
            .unwrap()
            .syntax()
            .descendants()
            .find_map(ast::MacroCall::cast)
            .unwrap();
        manifest.scope = Some(json!({"assertionIdentities":{
            "schema":"supercov-rust-assertion-identities-v1", "records":[
                record("call", "compute(21)", REAL, false),
                record("macro", &assertion.syntax().text().to_string(), "core::assert_eq", true),
            ]
        }}));
        (manifest, results, sources, fingerprint)
    }

    #[test]
    fn compiler_identity_distinguishes_a_renamed_import_from_a_same_named_hit() {
        for body in [
            "assert_eq!(compute(21), 42);",
            "let result = compute(21); let alias = result; assert_eq!(alias, 42);",
        ] {
            let (mut manifest, results, sources, fingerprint) = identity_fixture(body);
            let files = BTreeSet::from(["src/lib.rs".into()]);
            let analyzed =
                analyze_source_assertions(&manifest, &results, &files, &sources, &fingerprint)
                    .unwrap();
            assert_eq!(analyzed.witnesses.len(), 1);
            assert_eq!(analyzed.witnesses[0].point, REAL);
            assert!(analyzed.witnesses[0].compiler_resolved);
            manifest.scope = None;
            assert!(
                analyze_source_assertions(&manifest, &results, &files, &sources, &fingerprint)
                    .unwrap()
                    .witnesses
                    .is_empty()
            );
        }
    }

    #[test]
    fn missing_conflicting_and_nonstandard_compiler_identities_never_fall_back_to_spelling() {
        let (manifest, results, sources, fingerprint) =
            identity_fixture("assert_eq!(compute(21), 42);");
        let original = manifest.scope.as_ref().unwrap()["assertionIdentities"]["records"]
            .as_array()
            .unwrap();
        let mut conflict = original[0].clone();
        conflict["target"] = json!(DECOY);
        let mut nonstandard = original[1].clone();
        nonstandard["standardEquality"] = json!(false);
        let mut missing_target = original[0].clone();
        missing_target["target"] = json!("rs:function:000000000000000000000099");
        for records in [
            vec![],
            vec![original[0].clone()],
            vec![original[1].clone()],
            vec![original[0].clone(), original[1].clone(), conflict],
            vec![original[0].clone(), nonstandard],
            vec![missing_target, original[1].clone()],
        ] {
            let mut changed = manifest.clone();
            changed.scope.as_mut().unwrap()["assertionIdentities"]["records"] = json!(records);
            assert!(
                analyze_source_assertions(
                    &changed,
                    &results,
                    &BTreeSet::from(["src/lib.rs".into()]),
                    &sources,
                    &fingerprint
                )
                .unwrap()
                .witnesses
                .is_empty()
            );
        }
    }

    #[test]
    fn identity_source_and_owner_corruption_fail_closed() {
        let (manifest, results, sources, fingerprint) =
            identity_fixture("assert_eq!(compute(21), 42);");
        for (field, invalid) in [
            ("source", json!("compute(22)")),
            ("owner", json!(DECOY)),
            ("start", json!(0)),
            ("end", json!(10000)),
            ("unexpected", json!(true)),
        ] {
            let mut changed = manifest.clone();
            changed.scope.as_mut().unwrap()["assertionIdentities"]["records"][0][field] = invalid;
            assert!(
                analyze_source_assertions(
                    &changed,
                    &results,
                    &BTreeSet::from(["src/lib.rs".into()]),
                    &sources,
                    &fingerprint
                )
                .is_err(),
                "{field}"
            );
        }
    }

    fn analyze(library: &str, body: &str) -> RustSourceAssertionAnalysis {
        let (manifest, results, sources, fingerprint) = fixture(library, body);
        analyze_source_assertions(
            &manifest,
            &results,
            &BTreeSet::from(["src/lib.rs".into()]),
            &sources,
            &fingerprint,
        )
        .unwrap()
    }

    #[test]
    fn exact_direct_reversed_qualified_and_local_values() {
        for body in [
            "assert_eq!(f(21), 42);",
            "assert_eq!(42, f(21));",
            "std::assert_eq!(sample::f(-2), -4);",
            "let result = f(21); assert_eq!(result, 42);",
            "let result = f(1); let result = f(21); assert_eq!(result, 42);",
        ] {
            let output = analyze(LIB, body);
            assert_eq!(output.witnesses.len(), 1, "{body}: {:?}", output.limits);
            assert_eq!(join(&output.facts)[0].status, Status::Evident);
            assert_eq!(output.facts.sites[0].bounds[0].boundary, "return:f");
        }
    }

    #[test]
    fn bools_are_exact_but_floats_custom_equality_and_type_aliases_are_not_claimed() {
        assert_eq!(
            analyze(
                "pub fn f(x: i32) -> bool { x > 0 }",
                "assert_eq!(f(1), true);"
            )
            .witnesses
            .len(),
            1
        );
        for library in [
            "pub fn f(x: i32) -> f32 { x as f32 }",
            "pub struct Always; pub fn f(x: i32) -> Always { Always }",
            "type i32 = Always; pub struct Always; pub fn f(x: i32) -> i32 { x }",
        ] {
            assert!(
                analyze(library, "assert_eq!(f(1), 1);")
                    .witnesses
                    .is_empty()
            );
        }
    }

    #[test]
    fn discarded_masked_predicate_and_co_varying_expected_values_remain_unknown() {
        for body in [
            "assert_eq!({ f(21); 42 }, 42);",
            "assert_eq!(f(21) % 2, 0);",
            "assert_ne!(f(21), -999);",
            "assert_eq!(f(21), f(21));",
            "assert_eq!(f(f(21)), 84);",
            "assert_eq!(f(21), 42, \"message\");",
            "#[cfg(any())] assert_eq!(f(21), 42);",
        ] {
            assert!(analyze(LIB, body).witnesses.is_empty(), "{body}");
        }
    }

    #[test]
    fn mutable_bindings_items_and_cross_thread_transfers_are_not_traced() {
        for body in [
            "let mut result = f(21); assert_eq!(result, 42);",
            "let result = f(21); let result = 42; assert_eq!(result, 42);",
            "let result = std::thread::spawn(|| f(21)); assert_eq!(result.join().unwrap(), 42);",
            "assert_eq!(f(21), 42); fn f(_: i32) -> i32 { sample::f(21); 42 }",
            "assert_eq!(f(21), 42); macro_rules! assert_eq { ($($t:tt)*) => {} }",
        ] {
            assert!(analyze(LIB, body).witnesses.is_empty(), "{body}");
        }
    }

    #[test]
    fn immutable_copies_record_the_source_ordered_value_path() {
        let body = "let result = f(21);\nlet first = result;\nlet second = first;\nassert_eq!(42, second);";
        let output = analyze(LIB, body);
        assert_eq!(output.witnesses.len(), 1);
        let witness = &output.witnesses[0];
        assert!(witness.via_local);
        assert_eq!(witness.call, "f(21)");
        assert_eq!(witness.producer, "tests/t.rs:4:0");
        assert_eq!(witness.assertion, "tests/t.rs:7:0");
        assert_eq!(
            witness
                .bindings
                .iter()
                .map(|b| (b.name.as_str(), b.at.as_str(), b.initializer.as_str()))
                .collect::<Vec<_>>(),
            [
                ("result", "tests/t.rs:4:0", "f(21)"),
                ("first", "tests/t.rs:5:0", "result"),
                ("second", "tests/t.rs:6:0", "first"),
            ]
        );
        assert_eq!(join(&output.facts)[0].status, Status::Evident);
        let direct = analyze(LIB, "assert_eq!(f(21), 42);");
        assert!(direct.witnesses[0].bindings.is_empty());
        let boolean = analyze(
            "pub fn f(x: i32) -> bool { x > 0 }",
            "let result = f(1); let alias = result; assert_eq!(true, alias);",
        );
        assert_eq!(boolean.witnesses[0].primitive, "bool");
        assert_eq!(boolean.witnesses[0].bindings.len(), 2);
    }

    #[test]
    fn copies_preserve_binding_versions_across_self_and_source_shadowing() {
        for (body, call, initializers) in [
            (
                "let result = f(21); let result = result; assert_eq!(result, 42);",
                "f(21)",
                vec!["f(21)", "result"],
            ),
            (
                "let result = f(21); let alias = result; let result = f(2); let saved = alias; assert_eq!(saved, 42);",
                "f(21)",
                vec!["f(21)", "result", "alias"],
            ),
            (
                "let result = f(21); let alias = result; let alias = f(2); assert_eq!(alias, 4);",
                "f(2)",
                vec!["f(2)"],
            ),
            (
                "let result = f(21); let alias = result; let result = f(2); let alias = result; assert_eq!(alias, 4);",
                "f(2)",
                vec!["f(2)", "result"],
            ),
        ] {
            let output = analyze(LIB, body);
            assert_eq!(output.witnesses.len(), 1, "{body}");
            assert_eq!(output.witnesses[0].call, call, "{body}");
            assert_eq!(
                output.witnesses[0]
                    .bindings
                    .iter()
                    .map(|b| b.initializer.as_str())
                    .collect::<Vec<_>>(),
                initializers,
                "{body}"
            );
        }
    }

    #[test]
    fn aliases_do_not_bypass_unknown_rebindings_mutation_or_transformations() {
        for body in [
            "let result = f(21); let alias = result; let alias = 42; assert_eq!(alias, 42);",
            "let result = f(21); let alias = result; let result = 42; let alias = result; assert_eq!(alias, 42);",
            "let result = f(21); let mut alias = result; alias = 42; assert_eq!(alias, 42);",
            "let mut result = f(21); result = 42; let alias = result; assert_eq!(alias, 42);",
            "let result = f(21); let alias = result % 2; assert_eq!(alias, 0);",
            "let result = f(21); let alias = { let nested = result; nested }; assert_eq!(alias, 42);",
            "let result = f(21); let alias = &result; assert_eq!(*alias, 42);",
            "let result = f(21); let alias = result.clone(); assert_eq!(alias, 42);",
            "let result = f(21); let alias: i32 = result; assert_eq!(alias, 42);",
            "let result = f(21); let alias = f(2); #[cfg(any())] let alias = result; assert_eq!(alias, 4);",
            "let result = f(21); let alias = result; assert_eq!(alias, result);",
        ] {
            assert!(analyze(LIB, body).witnesses.is_empty(), "{body}");
        }
    }

    #[test]
    fn alias_provenance_cannot_be_borrowed_from_a_different_attempt() {
        let (manifest, mut results, sources, fingerprint) = fixture(
            LIB,
            "let result = f(21); let alias = result; assert_eq!(alias, 42);",
        );
        let mut other = results[0].clone();
        other.phases[0].status = None;
        results[0].runtime[0].hits.clear();
        results[0].runtime[0]
            .events
            .retain(|e| e.event_type != "hit");
        results.push(other);
        let output = analyze_source_assertions(
            &manifest,
            &results,
            &BTreeSet::from(["src/lib.rs".into()]),
            &sources,
            &fingerprint,
        )
        .unwrap();
        assert!(output.witnesses.is_empty());
        assert_eq!(output.facts.sites[0].covered_by.len(), 1);
    }

    #[test]
    fn long_self_shadowing_copy_chain_has_finite_source_ordered_provenance() {
        let mut body = "let result = f(21);\n".to_owned();
        body.push_str(&"let result = result;\n".repeat(1024));
        body.push_str("assert_eq!(result, 42);");
        let output = analyze(LIB, &body);
        assert_eq!(output.witnesses.len(), 1);
        assert_eq!(output.witnesses[0].bindings.len(), 1025);
        assert_eq!(output.witnesses[0].bindings[0].initializer, "f(21)");
        assert_eq!(output.witnesses[0].bindings[1024].at, "tests/t.rs:1028:0");
    }

    #[test]
    fn source_and_outcome_provenance_are_mandatory() {
        let (mut manifest, mut results, mut sources, fingerprint) =
            fixture(LIB, "assert_eq!(f(21), 42);");
        let files = BTreeSet::from(["src/lib.rs".into()]);
        sources.files.get_mut("src/lib.rs").unwrap().push(' ');
        assert!(
            analyze_source_assertions(&manifest, &results, &files, &sources, &fingerprint).is_err()
        );
        sources.files.get_mut("src/lib.rs").unwrap().pop();
        results[0].phases[0].status = None;
        assert!(
            analyze_source_assertions(&manifest, &results, &files, &sources, &fingerprint)
                .unwrap()
                .witnesses
                .is_empty()
        );
        results[0].phases[0].status = Some("passed".into());
        manifest.unmeasured.push("f".into());
        assert!(
            analyze_source_assertions(&manifest, &results, &files, &sources, &fingerprint)
                .unwrap()
                .witnesses
                .is_empty()
        );
    }

    #[test]
    fn exported_macro_shadows_and_nondecision_events_do_not_lend_witnesses() {
        let shadow =
            format!("{LIB}\n#[macro_export] macro_rules! assert_eq {{ ($($t:tt)*) => {{}} }}");
        assert!(
            analyze(&shadow, "assert_eq!(f(21), 42);")
                .witnesses
                .is_empty()
        );
        for helper in [
            "fn helper() { #[macro_export] macro_rules! assert_eq { ($($t:tt)*) => {} } }",
            "fn helper() { include!(\"macros.rs\"); }",
            "fn helper() { assert_eq!({ #[macro_export] macro_rules! assert_eq { ($($t:tt)*) => {} } 42 }, 42); }",
        ] {
            let (manifest, results, mut sources, mut fingerprint) =
                fixture(LIB, "assert_eq!(f(21), 42);");
            sources
                .files
                .get_mut("tests/t.rs")
                .unwrap()
                .push_str(helper);
            let mut hash = Sha256::new();
            for (path, source) in &sources.files {
                hash.update(path);
                hash.update([0]);
                hash.update(source);
                hash.update([0]);
            }
            let digest = format!("{:x}", hash.finalize());
            fingerprint.source = digest.clone();
            fingerprint.tests = digest;
            assert!(
                analyze_source_assertions(
                    &manifest,
                    &results,
                    &BTreeSet::from(["src/lib.rs".into()]),
                    &sources,
                    &fingerprint
                )
                .unwrap()
                .witnesses
                .is_empty(),
                "{helper}"
            );
        }
        let (manifest, mut results, sources, fingerprint) = fixture(LIB, "assert_eq!(f(21), 42);");
        for event in &mut results[0].runtime[0].events {
            if event.event_type == "decision" {
                event.event_type = "hit".into();
            }
        }
        assert!(
            analyze_source_assertions(
                &manifest,
                &results,
                &BTreeSet::from(["src/lib.rs".into()]),
                &sources,
                &fingerprint
            )
            .unwrap()
            .witnesses
            .is_empty()
        );
    }

    #[test]
    fn exact_value_observation_does_not_mean_all_function_mutations_are_killed() {
        let output = analyze(LIB, "assert_eq!(f(0), 0);");
        assert_eq!(output.witnesses.len(), 1);
        // Replacing the body with 0 or x / 2 preserves the observed value.
        // No field in this analysis claims those changes are killed.
        assert_eq!(output.witnesses[0].expected, "0");
    }
}
