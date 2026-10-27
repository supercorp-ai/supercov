//! Supercov-owned Rust parsing and obligation discovery.
//!
//! The first private frontend deliberately starts from a lossless concrete
//! syntax tree. LLVM/rustc coverage remains a development oracle; it is not a
//! product input. Probe insertion and Cargo execution build on this exact
//! source denominator.

use std::collections::{BTreeMap, BTreeSet};

use ra_ap_syntax::{
    AstNode, Edition, SourceFile, SyntaxKind, TextRange,
    ast::{self, BinaryOp, HasAttrs, HasLoopBody, HasName, LogicOp},
};
use serde_json::json;

use crate::{
    coverage_analysis::PointKind,
    coverage_report::{
        BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta, PointMeta,
    },
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustInstrumenterError {
    SourceTooLarge,
    Parse(Vec<String>),
    InvalidRange,
    InvalidRuntimePath,
    /// Supercov broke one of its own invariants on this file: a bug, with
    /// the message it panicked with.
    Internal(String),
}

impl std::fmt::Display for RustInstrumenterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceTooLarge => write!(formatter, "Rust source exceeds the parser range"),
            Self::Parse(errors) => write!(formatter, "Rust parse failed: {}", errors.join("; ")),
            Self::InvalidRange => write!(formatter, "Rust parser returned an invalid range"),
            Self::Internal(message) => write!(formatter, "an internal error: {message}"),
            Self::InvalidRuntimePath => write!(formatter, "invalid generated Rust runtime path"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RustInstrumentedSource {
    pub code: String,
    pub manifest: CoverageManifest,
    /// The numbering this file's probes used; empty when the caller numbers
    /// a whole workspace and holds the numbering itself.
    pub probes: RustProbeLayout,
}

/// The probes of an instrumentation, numbered: a probe call carries its
/// ordinal, and the region the runtime writes holds one slot per ordinal in
/// each space. The ids an ordinal stands for never cross the transport.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RustProbeLayout {
    /// The instrumentation's token: regions are named by it, and a region
    /// with another token is another instrumentation's.
    #[serde(default)]
    pub token: String,
    /// Statements, functions and branch alternatives: what `hit` records.
    pub hits: Vec<String>,
    /// Statements that assert, and assertion sites in test code.
    pub assertions: Vec<String>,
    pub decisions: Vec<String>,
    /// Code behind a `#[cfg]` the target alone does not settle -- a feature,
    /// `test`, a build script's `--cfg`. Each crate's runtime writes what
    /// `cfg!` says of the predicates in its own files when it maps the
    /// region, so the compiler that built the code decides.
    #[serde(default)]
    pub cfgs: Vec<RustCfgGate>,
    /// The files each crate compiles, by the key its region is exported
    /// under. A crate a test's Worker returned a region for was built --
    /// by `wrangler dev`, for wasm -- though the run's own build never
    /// compiled it, and its files count.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub crates: BTreeMap<String, Vec<String>>,
    /// The keys of `crates` that are Rust Workers' crates: built by
    /// worker-build when a test starts the Worker, and counted whether or not
    /// one did.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub workers: BTreeSet<String>,
}

/// Code a `#[cfg]` predicate keeps or leaves out, and the obligations in it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RustCfgGate {
    pub file: String,
    /// The predicate as `cfg!` takes it: `feature = "serde"`,
    /// `all(unix, not(test))`.
    pub predicate: String,
    /// The lines the gated code spans.
    pub line: usize,
    pub end_line: usize,
    pub obligations: Vec<String>,
}

#[derive(Default)]
struct ProbeNumbering {
    layout: RustProbeLayout,
    hits: std::collections::HashMap<String, u32>,
    assertions: std::collections::HashMap<String, u32>,
    decisions: std::collections::HashMap<String, u32>,
}

impl ProbeNumbering {
    fn from_layout(layout: RustProbeLayout) -> Self {
        let index = |ids: &[String]| {
            ids.iter()
                .enumerate()
                .map(|(ordinal, id)| (id.clone(), ordinal as u32))
                .collect()
        };
        Self {
            hits: index(&layout.hits),
            assertions: index(&layout.assertions),
            decisions: index(&layout.decisions),
            layout,
        }
    }

    fn number(
        ids: &mut Vec<String>,
        index: &mut std::collections::HashMap<String, u32>,
        id: &str,
    ) -> u32 {
        if let Some(ordinal) = index.get(id) {
            return *ordinal;
        }
        let ordinal = ids.len() as u32;
        ids.push(id.to_owned());
        index.insert(id.to_owned(), ordinal);
        ordinal
    }
}

thread_local! {
    /// The numbering of the instrumentation in progress on this thread:
    /// every probe site asks it for its ordinal, so the files of one
    /// workspace share one numbering without passing it to each site.
    static NUMBERING: std::cell::RefCell<ProbeNumbering> =
        std::cell::RefCell::new(ProbeNumbering::default());
}

fn hit_ordinal(id: &str) -> u32 {
    NUMBERING.with(|numbering| {
        let numbering = &mut *numbering.borrow_mut();
        ProbeNumbering::number(&mut numbering.layout.hits, &mut numbering.hits, id)
    })
}

fn assertion_ordinal(id: &str) -> u32 {
    NUMBERING.with(|numbering| {
        let numbering = &mut *numbering.borrow_mut();
        ProbeNumbering::number(
            &mut numbering.layout.assertions,
            &mut numbering.assertions,
            id,
        )
    })
}

fn cfg_gate_ordinal(gate: RustCfgGate) -> u32 {
    NUMBERING.with(|numbering| {
        let gates = &mut numbering.borrow_mut().layout.cfgs;
        gates.push(gate);
        (gates.len() - 1) as u32
    })
}

fn decision_ordinal(id: &str) -> u32 {
    NUMBERING.with(|numbering| {
        let numbering = &mut *numbering.borrow_mut();
        ProbeNumbering::number(
            &mut numbering.layout.decisions,
            &mut numbering.decisions,
            id,
        )
    })
}

/// Hands this thread's numbering back when an instrumentation ends, however
/// it ends.
struct NumberingScope<'a> {
    layout: &'a mut RustProbeLayout,
}

impl<'a> NumberingScope<'a> {
    fn enter(layout: &'a mut RustProbeLayout) -> Self {
        let taken = std::mem::take(layout);
        NUMBERING.with(|numbering| *numbering.borrow_mut() = ProbeNumbering::from_layout(taken));
        Self { layout }
    }
}

impl Drop for NumberingScope<'_> {
    fn drop(&mut self) {
        *self.layout =
            NUMBERING.with(|numbering| std::mem::take(&mut *numbering.borrow_mut()).layout);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InsertionKind {
    End,
    Direct,
    Start,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Insertion {
    offset: usize,
    kind: InsertionKind,
    scope_len: usize,
    rank: usize,
    text: String,
}

fn valid_runtime_path(path: &str) -> bool {
    let mut parts = path.split("::");
    if !matches!(parts.next(), Some("crate")) {
        return false;
    }
    let parts = parts.collect::<Vec<_>>();
    !parts.is_empty()
        && parts.into_iter().all(|part| {
            !part.is_empty()
                && part.bytes().enumerate().all(|(index, byte)| {
                    byte == b'_'
                        || byte.is_ascii_alphabetic()
                        || (index > 0 && byte.is_ascii_digit())
                })
        })
}

/// Report whether rustc will evaluate this node at compile time.
///
/// Runtime probes cannot appear anywhere this is true: `condition`, `decision`
/// and `hit` are not `const fn`, so emitting a call here is not a bad
/// measurement but a build failure (E0015). bytes-1.12.1 hit exactly that with
/// `const ITERS: usize = if cfg!(miri) { 100 } else { 1_000 };`.
///
/// `ConstArg` is the shared node for enum discriminants, array lengths, const
/// generic arguments and const parameter defaults, so matching it covers all
/// four. The remaining case is an array repeat expression, `[value; count]`,
/// where only the count after the semicolon is const-evaluated.
/// Report whether the source's doc comments contain a fenced code block.
///
/// rustdoc turns fenced blocks in `///`, `//!` and `#[doc]` text into doctest
/// crates. The scan is line-based and deliberately coarse: a fence inside a
/// doc comment declares the limitation even when the fence is `ignore`d, which
/// over-declares the unmeasured surface rather than ever under-declaring it.
fn in_const_context(node: &ra_ap_syntax::SyntaxNode) -> bool {
    let start = node.text_range().start();
    node.ancestors().any(|ancestor| {
        ast::Fn::cast(ancestor.clone()).is_some_and(|function| function.const_token().is_some())
            || ast::BlockExpr::cast(ancestor.clone())
                .is_some_and(|block| block.const_token().is_some())
            || ast::Const::can_cast(ancestor.kind())
            || ast::Static::can_cast(ancestor.kind())
            || ast::ConstArg::can_cast(ancestor.kind())
            || ast::ArrayExpr::cast(ancestor).is_some_and(|array| {
                array
                    .semicolon_token()
                    .is_some_and(|semicolon| start >= semicolon.text_range().end())
            })
    })
}

/// Report whether this node sits inside a `GlobalAlloc` implementation.
///
/// The probe runtime allocates, so a probe inside `alloc` calls back into
/// `alloc`, which probes again, until the stack is gone. bytes-1.12.1's
/// tests/test_bytes_odd_alloc.rs installs a `#[global_allocator]`, and the
/// instrumented binary died with SIGSEGV before libtest could even list its
/// tests -- while the uninstrumented one listed them fine.
///
/// The general rule this enforces is that nothing the runtime itself calls can
/// carry a probe, and `#[global_allocator]` is the one way a user crate gets
/// onto that path. A `GlobalAlloc` impl is skipped whether or not it is the
/// registered allocator, because the registering `static` may live in another
/// file: declining a handful of allocator bodies costs almost no exactness,
/// while instrumenting the live one costs the whole run.
fn in_global_allocator(node: &ra_ap_syntax::SyntaxNode) -> bool {
    node.ancestors()
        .any(|ancestor| in_global_allocator_impl(&ancestor))
}

/// Report whether this `impl` block implements `GlobalAlloc`.
fn in_global_allocator_impl(node: &ra_ap_syntax::SyntaxNode) -> bool {
    ast::Impl::cast(node.clone()).is_some_and(|block| {
        block.trait_().is_some_and(|implemented| {
            implemented
                .syntax()
                .descendants_with_tokens()
                .filter_map(|element| element.into_token())
                .any(|token| token.kind() == SyntaxKind::IDENT && token.text() == "GlobalAlloc")
        })
    })
}

/// Whether a `for` loop over `iterable` runs its body at least once, by its
/// text alone: a non-empty array literal, a constant of the file that is
/// one, a range between integer literals that holds a value, or a `split`,
/// which yields a piece even of nothing, read through references,
/// parentheses and the adapters that keep every item. Such a loop has no
/// zero-iterations outcome to observe, as a match's last arm has no skipped
/// one; the JavaScript instrumenter reads array literals the same way.
fn never_empty(iterable: &ast::Expr) -> bool {
    never_empty_naming(iterable, true)
}

/// A decimal integer literal's value, its `_`s and type suffix dropped.
fn literal_integer(literal: &ast::Literal) -> Option<u128> {
    let text = literal.syntax().text().to_string();
    let digits = text
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '_')
        .filter(|c| *c != '_')
        .collect::<String>();
    let suffix = &text[text.len().min(digits.len() + text.matches('_').count())..];
    (suffix.is_empty() || suffix.starts_with(['u', 'i']))
        .then(|| digits.parse::<u128>().ok())
        .flatten()
}

/// The name a path is, when it is a bare one.
fn bare_name(path: &ast::PathExpr) -> Option<ast::NameRef> {
    path.path()
        .filter(|path| path.qualifier().is_none())
        .and_then(|path| path.segment())
        .and_then(|segment| segment.name_ref())
}

/// The values of every `const` and `static` named `name` in the file that
/// holds `node`.
fn constant_bodies(node: &ast::Expr, name: &ast::NameRef) -> impl Iterator<Item = ast::Expr> {
    let file = node.syntax().ancestors().last();
    let file = file.expect("a node is among its own ancestors");
    let name = name.text().to_string();
    file.descendants().filter_map(move |item| {
        let (named, body) = match_constant(item)?;
        (named.text() == name).then_some(body).flatten()
    })
}

/// A `const` or `static` item's name and value.
fn match_constant(item: ra_ap_syntax::SyntaxNode) -> Option<(ast::Name, Option<ast::Expr>)> {
    if let Some(constant) = ast::Const::cast(item.clone()) {
        return Some((constant.name()?, constant.body()));
    }
    let item = ast::Static::cast(item)?;
    Some((item.name()?, item.body()))
}

/// [`never_empty`], following a constant's name when `constants` is set: a
/// constant's own value is read without, so a cycle ends.
fn never_empty_naming(iterable: &ast::Expr, constants: bool) -> bool {
    use ast::RangeItem;
    // A bound written as a decimal integer, or, by its bare name, a
    // `const` of this file that is one: `1..=MOST_COPIES`.
    let integer = |bound: Option<ast::Expr>| match bound {
        Some(ast::Expr::Literal(literal)) => literal_integer(&literal),
        Some(ast::Expr::PathExpr(path)) if constants => {
            let name = bare_name(&path)?;
            constant_bodies(iterable, &name).find_map(|body| match body {
                ast::Expr::Literal(literal) => literal_integer(&literal),
                _ => None,
            })
        }
        _ => None,
    };
    match iterable {
        // `[x; n]` may be `[x; 0]`.
        ast::Expr::ArrayExpr(array) => {
            array.semicolon_token().is_none() && array.exprs().next().is_some()
        }
        ast::Expr::RangeExpr(range) => match (integer(range.start()), integer(range.end())) {
            (Some(start), Some(end)) => match range.op_kind() {
                Some(ast::RangeOp::Inclusive) => start <= end,
                _ => start < end,
            },
            _ => false,
        },
        ast::Expr::ParenExpr(inner) => inner
            .expr()
            .is_some_and(|inner| never_empty_naming(&inner, constants)),
        ast::Expr::RefExpr(inner) => inner
            .expr()
            .is_some_and(|inner| never_empty_naming(&inner, constants)),
        // A `const` or `static` of this file, by its bare name.
        ast::Expr::PathExpr(path) if constants => bare_name(path).is_some_and(|name| {
            constant_bodies(iterable, &name).any(|body| never_empty_naming(&body, false))
        }),
        ast::Expr::MethodCallExpr(call)
            if call
                .name_ref()
                .is_some_and(|name| name.text() == "split" || name.text() == "rsplit") =>
        {
            true
        }
        // `splitn(0, ..)` yields nothing; any other count, its first piece.
        ast::Expr::MethodCallExpr(call)
            if call
                .name_ref()
                .is_some_and(|name| name.text() == "splitn" || name.text() == "rsplitn") =>
        {
            ast::HasArgList::arg_list(call)
                .and_then(|arguments| arguments.args().next())
                .and_then(|count| integer(Some(count)))
                .is_some_and(|count| count > 0)
        }
        ast::Expr::MethodCallExpr(call) => {
            call.name_ref().is_some_and(|name| {
                matches!(
                    name.text(),
                    "iter"
                        | "into_iter"
                        | "iter_mut"
                        | "enumerate"
                        | "copied"
                        | "cloned"
                        | "map"
                        | "rev"
                )
            }) && call
                .receiver()
                .is_some_and(|receiver| never_empty_naming(&receiver, constants))
        }
        _ => false,
    }
}

/// Report whether a probe placed at this node could not run correctly.
fn cannot_carry_probe(node: &ra_ap_syntax::SyntaxNode) -> bool {
    in_const_context(node) || in_global_allocator(node)
}

fn range_offsets(range: TextRange) -> (usize, usize) {
    (usize::from(range.start()), usize::from(range.end()))
}

fn push_wrapper(
    insertions: &mut Vec<Insertion>,
    range: TextRange,
    scope: TextRange,
    rank: usize,
    prefix: String,
    suffix: String,
) {
    let (start, end) = range_offsets(range);
    let (scope_start, scope_end) = range_offsets(scope);
    let scope_len = scope_end - scope_start;
    insertions.push(Insertion {
        offset: start,
        kind: InsertionKind::Start,
        scope_len,
        rank,
        text: prefix,
    });
    insertions.push(Insertion {
        offset: end,
        kind: InsertionKind::End,
        scope_len,
        rank,
        text: suffix,
    });
}

/// Spaces in place of `bytes`, their line breaks kept so every line stays
/// where it was.
fn blank(bytes: &mut [u8]) {
    bytes
        .iter_mut()
        .filter(|byte| **byte != b'\n')
        .for_each(|byte| *byte = b' ');
}

fn push_direct(insertions: &mut Vec<Insertion>, offset: usize, text: String) {
    insertions.push(Insertion {
        offset,
        kind: InsertionKind::Direct,
        scope_len: 0,
        rank: 0,
        text,
    });
}

fn apply_insertions(
    source: &str,
    insertions: Vec<Insertion>,
) -> Result<String, RustInstrumenterError> {
    if insertions
        .iter()
        .any(|edit| edit.offset > source.len() || !source.is_char_boundary(edit.offset))
    {
        return Err(RustInstrumenterError::InvalidRange);
    }
    // Two wrappers of one range with the same scope and rank nest in the
    // order they were pushed: the first opens first and closes last. Left to
    // a stable sort alone they crossed, `{ a ({ b ) } }`, and an attributed
    // `let` whose initializer is a decision did not parse.
    let mut insertions = insertions.into_iter().enumerate().collect::<Vec<_>>();
    insertions.sort_by(|(left_order, left), (right_order, right)| {
        left.offset.cmp(&right.offset).then_with(|| {
            let kind_order = |kind: InsertionKind| match kind {
                InsertionKind::End => 0,
                InsertionKind::Direct => 1,
                InsertionKind::Start => 2,
            };
            kind_order(left.kind)
                .cmp(&kind_order(right.kind))
                .then_with(|| match left.kind {
                    InsertionKind::End => left
                        .scope_len
                        .cmp(&right.scope_len)
                        .then_with(|| right.rank.cmp(&left.rank))
                        .then_with(|| right_order.cmp(left_order)),
                    InsertionKind::Direct => left_order.cmp(right_order),
                    InsertionKind::Start => right
                        .scope_len
                        .cmp(&left.scope_len)
                        .then_with(|| left.rank.cmp(&right.rank))
                        .then_with(|| left_order.cmp(right_order)),
                })
        })
    });
    let insertions = insertions
        .into_iter()
        .map(|(_, insertion)| insertion)
        .collect::<Vec<_>>();

    let mut output = source.to_owned();
    let mut index = insertions.len();
    while index > 0 {
        let offset = insertions[index - 1].offset;
        let start = insertions[..index].partition_point(|insertion| insertion.offset < offset);
        let text = insertions[start..index]
            .iter()
            .map(|insertion| insertion.text.as_str())
            .collect::<String>();
        output.insert_str(offset, &text);
        index = start;
    }
    Ok(output)
}

/// A limitation record's ID. One record stands for one kind in one file, and
/// the protocol requires every record's ID to be unique across the manifest,
/// so the file is part of it.
fn limitation_id(kind: &str, file: &str) -> String {
    format!("{kind}#{file}")
}

/// A file-wide limitation. Its one caller adds it once per file, to a
/// manifest whose limitations are all site limitations of other kinds, so
/// its ID is never there already.
fn add_manifest_limitation(manifest: &mut CoverageManifest, file: &str, id: &str, reason: &str) {
    manifest.limitations.push(json!({
        "id": limitation_id(id, file),
        "kind": "source-scope",
        "file": file,
        "line": 1,
        "column": 0,
        "source": "",
        "reason": reason,
        // A shape the probes cannot reach is a boundary of the denominator,
        // not a failure to measure what is inside it.
        "blocking": false
    }));
}

fn allocate_frame_name(
    file: &str,
    condition: &ast::Expr,
    kind: &str,
    identifiers: &mut BTreeSet<String>,
) -> String {
    let id = stable_id(file, "decision", condition.syntax().text_range(), kind);
    let suffix = id.rsplit(':').next().unwrap_or("decision");
    let base = format!("__supercov_decision_{suffix}");
    let mut candidate = base.clone();
    let mut attempt = 0_usize;
    while !identifiers.insert(candidate.clone()) {
        attempt += 1;
        candidate = format!("{base}_{attempt}");
    }
    candidate
}

/// The `const` naming a match's alternative IDs. Upper case, so it raises no
/// naming lint in a crate that denies warnings.
fn allocate_table_name(
    file: &str,
    expression: &ast::MatchExpr,
    identifiers: &mut BTreeSet<String>,
) -> String {
    let id = stable_id(file, "match", expression.syntax().text_range(), "arms");
    let suffix = id
        .rsplit(':')
        .next()
        .unwrap_or("match")
        .to_ascii_uppercase();
    let base = format!("__SUPERCOV_ARMS_{suffix}");
    let mut candidate = base.clone();
    let mut attempt = 0_usize;
    while !identifiers.insert(candidate.clone()) {
        attempt += 1;
        candidate = format!("{base}_{attempt}");
    }
    candidate
}

/// The local that remembers whether a `while` loop has run its body.
fn allocate_flag_name(
    file: &str,
    expression: &ast::WhileExpr,
    identifiers: &mut BTreeSet<String>,
) -> String {
    let id = stable_id(file, "loop", expression.syntax().text_range(), "flag");
    let suffix = id.rsplit(':').next().unwrap_or("loop");
    let base = format!("__supercov_loop_{suffix}");
    let mut candidate = base.clone();
    let mut attempt = 0_usize;
    while !identifiers.insert(candidate.clone()) {
        attempt += 1;
        candidate = format!("{base}_{attempt}");
    }
    candidate
}

impl std::error::Error for RustInstrumenterError {}

struct SourceLocations<'a> {
    source: &'a str,
    line_starts: Vec<usize>,
}

impl<'a> SourceLocations<'a> {
    fn new(source: &'a str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(
            source
                .bytes()
                .enumerate()
                .filter_map(|(index, byte)| (byte == b'\n').then_some(index + 1)),
        );
        Self {
            source,
            line_starts,
        }
    }

    fn range(&self, range: TextRange) -> Result<(usize, usize), RustInstrumenterError> {
        // A `TextRange` never ends before it starts.
        let start = usize::from(range.start());
        let end = usize::from(range.end());
        if end > self.source.len()
            || !self.source.is_char_boundary(start)
            || !self.source.is_char_boundary(end)
        {
            return Err(RustInstrumenterError::InvalidRange);
        }
        Ok((start, end))
    }

    fn line_column(&self, offset: usize) -> (usize, usize) {
        let line_index = self.line_starts.partition_point(|start| *start <= offset) - 1;
        (line_index + 1, offset - self.line_starts[line_index])
    }

    fn text(&self, range: TextRange) -> Result<String, RustInstrumenterError> {
        let (start, end) = self.range(range)?;
        Ok(self.source[start..end].trim().to_owned())
    }
}

fn stable_id(file: &str, kind: &str, range: TextRange, suffix: &str) -> String {
    let start = usize::from(range.start()).to_string();
    let end = usize::from(range.end()).to_string();
    let encoded = crate::stable_ids::digest(&[file, kind, &start, &end, suffix]);
    format!("rs:{kind}:{encoded}")
}

struct RustObligationCollector<'a> {
    file: &'a str,
    locations: SourceLocations<'a>,
    manifest: CoverageManifest,
    point_ids: BTreeSet<String>,
    decision_ids: BTreeSet<String>,
    branch_ids: BTreeSet<String>,
    /// Limitation kinds already declared for this file.
    site_limitations: BTreeSet<&'static str>,
    /// Where each obligation sits, so the ones no probe can reach can be
    /// declined once the whole file has been read.
    obligation_ranges: Vec<(String, TextRange)>,
    /// Test code in this file, which is not measured.
    tests: Vec<TextRange>,
    /// Items the target's cfg leaves uncompiled, which no test can reach.
    compiled_out: Vec<TextRange>,
    /// Ranges a `#[cfg]` the target does not settle may leave out; `collect`
    /// fills `gated` with the obligations inside each.
    gates: Vec<TextRange>,
    gated: Vec<Vec<String>>,
    error: Option<RustInstrumenterError>,
}

impl<'a> RustObligationCollector<'a> {
    fn new(file: &'a str, source: &'a str) -> Self {
        Self {
            file,
            locations: SourceLocations::new(source),
            manifest: CoverageManifest {
                unmeasured: Vec::new(),
                decisions: Vec::new(),
                points: Vec::new(),
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
                assertion_sites: Vec::new(),
            },
            point_ids: BTreeSet::new(),
            decision_ids: BTreeSet::new(),
            branch_ids: BTreeSet::new(),
            site_limitations: BTreeSet::new(),
            obligation_ranges: Vec::new(),
            tests: Vec::new(),
            compiled_out: Vec::new(),
            gates: Vec::new(),
            gated: Vec::new(),
            error: None,
        }
    }

    fn leaving_out(mut self, tests: Vec<TextRange>, compiled_out: Vec<TextRange>) -> Self {
        self.tests = tests;
        self.compiled_out = compiled_out;
        self
    }

    fn gating(mut self, gates: Vec<TextRange>) -> Self {
        self.gates = gates;
        self
    }

    /// The obligations the manifest kept inside each gate, in order.
    fn gate_obligations(&mut self) {
        let kept = self
            .manifest
            .points
            .iter()
            .map(|point| point.id.as_str())
            .chain(
                self.manifest
                    .decisions
                    .iter()
                    .map(|decision| decision.id.as_str()),
            )
            .chain(
                self.manifest
                    .branches
                    .iter()
                    .map(|branch| branch.id.as_str()),
            )
            .collect::<BTreeSet<_>>();
        self.gated = self
            .gates
            .iter()
            .map(|gate| {
                let mut ids = self
                    .obligation_ranges
                    .iter()
                    .filter(|(id, range)| gate.contains_range(*range) && kept.contains(id.as_str()))
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                ids.sort();
                ids.dedup();
                ids
            })
            .collect();
    }

    /// Test code is not measured, as the usual Rust coverage tools leave it
    /// out by default. Its obligations go, and its statements stay only as the places an
    /// assertion marker may name.
    fn leave_out_test_code(&mut self) {
        // Code the compiler never builds goes with no trace: nothing in it
        // runs, its assertions included.
        let uncompiled = self
            .obligation_ranges
            .iter()
            .filter(|(_, range)| in_test_code(&self.compiled_out, *range))
            .map(|(id, _)| id.clone())
            .collect::<BTreeSet<_>>();
        let inside = self
            .obligation_ranges
            .iter()
            .filter(|(_, range)| in_test_code(&self.tests, *range))
            .map(|(id, _)| id.clone())
            .chain(uncompiled.iter().cloned())
            .collect::<BTreeSet<_>>();
        if inside.is_empty() {
            return;
        }
        for point in std::mem::take(&mut self.manifest.points) {
            if !inside.contains(&point.id) {
                self.manifest.points.push(point);
            } else if point.kind == PointKind::Statement && !uncompiled.contains(&point.id) {
                self.manifest.assertion_sites.push(point);
            }
        }
        self.manifest
            .decisions
            .retain(|decision| !inside.contains(&decision.id));
        self.manifest
            .branches
            .retain(|branch| !inside.contains(&branch.id));
        self.manifest.unmeasured.retain(|id| !inside.contains(id));
    }

    fn location_source(&mut self, range: TextRange) -> Option<(usize, usize, String)> {
        let result = self.locations.range(range).map(|(start, _)| {
            let (line, column) = self.locations.line_column(start);
            (line, column, self.locations.text(range))
        });
        match result {
            Ok((line, column, Ok(source))) => Some((line, column, source)),
            Ok((_, _, Err(error))) | Err(error) => {
                self.error.get_or_insert(error);
                None
            }
        }
    }

    fn point(&mut self, range: TextRange, kind: PointKind, label: Option<String>) {
        self.point_located(range, range, kind, label);
    }

    /// A point identified by `range` but reported at `location`: a function
    /// is identified by its whole node, which begins at its doc comments and
    /// attributes, and reported where the function itself starts.
    fn point_located(
        &mut self,
        range: TextRange,
        location: TextRange,
        kind: PointKind,
        label: Option<String>,
    ) {
        let kind_name = match kind {
            PointKind::Statement => "statement",
            PointKind::Function => "function",
        };
        let id = stable_id(self.file, kind_name, range, label.as_deref().unwrap_or(""));
        self.obligation_ranges.push((id.clone(), range));
        if !self.point_ids.insert(id.clone()) {
            return;
        }
        let Some((line, column, source)) = self.location_source(location) else {
            return;
        };
        self.manifest.points.push(PointMeta {
            id,
            kind,
            file: self.file.into(),
            line,
            column,
            source,
            label,
        });
    }

    fn atomic_condition_ranges(expression: &ast::Expr, ranges: &mut Vec<TextRange>) {
        match expression {
            ast::Expr::ParenExpr(paren) => {
                if let Some(inner) = paren.expr() {
                    Self::atomic_condition_ranges(&inner, ranges);
                } else {
                    ranges.push(expression.syntax().text_range());
                }
            }
            ast::Expr::BinExpr(binary)
                if matches!(
                    binary.op_kind(),
                    Some(BinaryOp::LogicOp(LogicOp::And | LogicOp::Or))
                ) =>
            {
                // Only a tree with errors lacks an operand: `(a &&)`. Its
                // operator is then one condition, as any other expression.
                if let (Some(left), Some(right)) = (binary.lhs(), binary.rhs()) {
                    Self::atomic_condition_ranges(&left, ranges);
                    Self::atomic_condition_ranges(&right, ranges);
                } else {
                    ranges.push(expression.syntax().text_range());
                }
            }
            _ => ranges.push(expression.syntax().text_range()),
        }
    }

    fn decision(&mut self, test: &ast::Expr, kind: &str) {
        let range = test.syntax().text_range();
        let id = stable_id(self.file, "decision", range, kind);
        self.obligation_ranges.push((id.clone(), range));
        if !self.decision_ids.insert(id.clone()) {
            return;
        }
        let Some((line, column, source)) = self.location_source(range) else {
            return;
        };
        let mut condition_ranges = Vec::new();
        Self::atomic_condition_ranges(test, &mut condition_ranges);
        let conditions = match condition_ranges
            .into_iter()
            .map(|condition| self.locations.text(condition))
            .collect::<Result<Vec<_>, _>>()
        {
            Ok(conditions) => conditions,
            Err(error) => {
                self.error.get_or_insert(error);
                return;
            }
        };
        self.manifest.decisions.push(DecisionMeta {
            id: id.clone(),
            file: self.file.into(),
            line,
            column,
            source: source.clone(),
            conditions,
            kind: kind.into(),
        });
        self.branch_with_id(
            format!("{id}:outcome"),
            range,
            kind,
            source,
            [("true", "true"), ("false", "false")],
        );
    }

    fn branch<const N: usize>(
        &mut self,
        range: TextRange,
        kind: &str,
        alternatives: [(&str, &str); N],
    ) {
        let id = stable_id(self.file, "branch", range, kind);
        let Some((_, _, source)) = self.location_source(range) else {
            return;
        };
        self.branch_with_id(id, range, kind, source, alternatives);
    }

    fn branch_with_id<const N: usize>(
        &mut self,
        id: String,
        range: TextRange,
        kind: &str,
        source: String,
        alternatives: [(&str, &str); N],
    ) {
        self.obligation_ranges.push((id.clone(), range));
        if !self.branch_ids.insert(id.clone()) {
            return;
        }
        let Some((line, column, _)) = self.location_source(range) else {
            return;
        };
        self.manifest.branches.push(BranchMeta {
            id: id.clone(),
            kind: kind.into(),
            file: self.file.into(),
            line,
            column,
            source,
            alternatives: alternatives
                .into_iter()
                .map(|(suffix, label)| BranchAlternativeMeta {
                    id: format!("{id}:{suffix}"),
                    label: label.into(),
                })
                .collect(),
        });
    }

    /// Obligations inside a region no probe can reach leave the denominator.
    /// They stay in the manifest -- the evidence files are named by a token
    /// over its obligation IDs, and the report still needs a line to hang the
    /// limitation on -- and are reported as unmeasured rather than uncovered.
    fn decline_unreachable_obligations(&mut self, root: &ra_ap_syntax::SyntaxNode) {
        // Exactly where no probe can be placed -- compile-time code, array
        // lengths and const arguments included -- nothing can be recorded,
        // so nothing is owed.
        let mut declined = self
            .manifest
            .unmeasured
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        for (id, range) in &self.obligation_ranges {
            let node = match root.covering_element(*range) {
                ra_ap_syntax::NodeOrToken::Node(node) => Some(node),
                ra_ap_syntax::NodeOrToken::Token(token) => token.parent(),
            };
            if node.is_some_and(|node| cannot_carry_probe(&node)) {
                declined.insert(id.clone());
            }
        }
        self.manifest.unmeasured = declined.into_iter().collect();
    }

    /// One limitation for one kind in this file, at the first site it hides.
    /// The report can then point at a line, and the file listing counts it
    /// against the file it belongs to. `kind` is "source-scope", which the
    /// index knows for code outside the measured denominator; anything else
    /// it renders as "unknown".
    fn site_limitation(&mut self, id: &'static str, range: TextRange, reason: String) {
        if self.site_limitations.contains(id)
            || in_test_code(&self.tests, range)
            || in_test_code(&self.compiled_out, range)
        {
            return;
        }
        let Some((line, column, source)) = self.location_source(range) else {
            return;
        };
        self.site_limitations.insert(id);
        // The first line of the site is enough to recognise it; a macro
        // invocation can run to dozens of lines.
        let source = source.lines().next().unwrap_or("").trim();
        let source = if source.chars().count() > 120 {
            format!("{}...", source.chars().take(117).collect::<String>())
        } else {
            source.to_owned()
        };
        self.manifest.limitations.push(json!({
            "id": limitation_id(id, self.file),
            "kind": "source-scope",
            "file": self.file,
            "line": line,
            "column": column,
            "source": source,
            // These are permanent boundaries of source instrumentation: the
            // obligations they hide are outside the denominator, not
            // unmeasured within it.
            "blocking": false,
            "reason": reason
        }));
    }

    fn collect(
        self,
        file: &SourceFile,
        assertions: &[TextRange],
        matches: &[TextRange],
    ) -> Result<CoverageManifest, RustInstrumenterError> {
        self.collect_gated(file, assertions, matches)
            .map(|(manifest, _)| manifest)
    }

    /// `collect`, with the obligations inside each of the `gating` ranges.
    fn collect_gated(
        mut self,
        file: &SourceFile,
        assertions: &[TextRange],
        matches: &[TextRange],
    ) -> Result<(CoverageManifest, Vec<Vec<String>>), RustInstrumenterError> {
        let root = file.syntax();

        for list in root.descendants().filter_map(ast::StmtList::cast) {
            for statement in list.statements() {
                match statement {
                    ast::Stmt::ExprStmt(statement) => {
                        self.point(statement.syntax().text_range(), PointKind::Statement, None);
                    }
                    ast::Stmt::LetStmt(statement) => {
                        self.point(statement.syntax().text_range(), PointKind::Statement, None);
                    }
                    ast::Stmt::Item(_) => {}
                }
            }
            if let Some(tail) = list.tail_expr() {
                self.point(tail.syntax().text_range(), PointKind::Statement, None);
            }
        }

        for function in root.descendants().filter_map(ast::Fn::cast) {
            if function.body().is_none() {
                continue;
            }
            if function.const_token().is_some() {
                self.site_limitation(
                    "rust-const-context-not-instrumented",
                    function.syntax().text_range(),
                    "Runtime probes cannot execute in const fn or compile-time evaluation".into(),
                );
                continue;
            }
            let label = function.name().map(|name| name.text().to_string());
            self.point_located(
                function.syntax().text_range(),
                item_range(function.syntax()),
                PointKind::Function,
                label,
            );
        }

        for closure in root.descendants().filter_map(ast::ClosureExpr::cast) {
            self.point(
                closure.syntax().text_range(),
                PointKind::Function,
                Some("<closure>".into()),
            );
        }

        for expression in root.descendants().filter_map(ast::IfExpr::cast) {
            if let Some(condition) = expression.condition() {
                self.decision(&condition, "if");
            }
        }
        for expression in root.descendants().filter_map(ast::WhileExpr::cast) {
            if let Some(condition) = expression.condition() {
                self.decision(&condition, "while");
            }
            self.branch(
                expression.syntax().text_range(),
                "while-loop",
                [("zero", "zero iterations"), ("entered", "entered")],
            );
        }
        for guard in root.descendants().filter_map(ast::MatchGuard::cast) {
            if let Some(condition) = guard.condition() {
                self.decision(&condition, "match-guard");
            }
        }
        for arguments in assertions {
            if let Some(condition) = assertion_condition(root, *arguments) {
                self.decision(&condition, "assert");
            }
        }
        for expression in standalone_matches(root, matches, assertions) {
            self.decision(&expression, "matches");
        }
        for expression in value_decisions(root, assertions) {
            self.decision(&expression, "value");
        }

        for binary in root.descendants().filter_map(ast::BinExpr::cast) {
            let kind = match binary.op_kind() {
                Some(BinaryOp::LogicOp(LogicOp::And)) => "logical-and",
                Some(BinaryOp::LogicOp(LogicOp::Or)) => "logical-or",
                _ => continue,
            };
            let range = binary.rhs().map_or_else(
                || binary.syntax().text_range(),
                |right| right.syntax().text_range(),
            );
            self.branch(
                range,
                kind,
                [
                    ("short-circuit", "short-circuited"),
                    ("evaluated", "right operand evaluated"),
                ],
            );
        }

        for expression in root.descendants().filter_map(ast::ForExpr::cast) {
            let range = expression.syntax().text_range();
            if expression
                .iterable()
                .is_some_and(|iterable| never_empty(&iterable))
            {
                self.branch(range, "for-loop", [("entered", "entered")]);
            } else {
                self.branch(
                    range,
                    "for-loop",
                    [("zero", "zero iterations"), ("entered", "entered")],
                );
            }
        }
        for expression in root.descendants().filter_map(ast::MatchExpr::cast) {
            let Some(list) = expression.match_arm_list() else {
                continue;
            };
            let arms = list.arms().collect::<Vec<_>>();
            let last = arms.len().saturating_sub(1);
            for (index, arm) in arms.iter().enumerate() {
                let range = arm.syntax().text_range();
                if index == last {
                    // A match is exhaustive, so once every earlier arm has
                    // been passed over the last one is selected: it can be
                    // reached but never skipped.
                    self.branch(range, "match-arm", [("selected", "selected")]);
                } else {
                    self.branch(
                        range,
                        "match-arm",
                        [("missed", "not selected"), ("selected", "selected")],
                    );
                }
            }
        }
        for expression in root.descendants().filter_map(ast::TryExpr::cast) {
            self.branch(
                expression.syntax().text_range(),
                "try-operator",
                [("continued", "continued"), ("returned", "early return")],
            );
        }

        // Only macros the view could not open remain: anything but the std
        // expression macros. One limitation for the file, at the first call
        // site, naming the macros it stands for: one per call site would let
        // a file full of `bail!` outrank every real gap, and the file-wide
        // entry this replaces sat at line 1 and named nothing.
        let mut macro_names = BTreeSet::new();
        let mut first_macro = None;
        let mut macro_sites = 0;
        for call in root.descendants().filter_map(ast::MacroCall::cast) {
            macro_names.insert(
                call.path()
                    .map_or("?".into(), |path| path.syntax().text().to_string()),
            );
            first_macro.get_or_insert_with(|| call.syntax().text_range());
            macro_sites += 1;
        }
        if let Some(range) = first_macro {
            let named = macro_names
                .iter()
                .take(4)
                .map(|name| format!("`{name}!`"))
                .collect::<Vec<_>>()
                .join(", ");
            let rest = macro_names.len().saturating_sub(4);
            self.site_limitation(
                "rust-macro-expansion-not-instrumented",
                range,
                format!(
                    "{named}{} expand in the compiler: their arguments and expansions are outside the owned source denominator ({macro_sites} call site{} in this file)",
                    if rest == 0 {
                        String::new()
                    } else {
                        format!(" and {rest} other macro{}", if rest == 1 { "" } else { "s" })
                    },
                    if macro_sites == 1 { "" } else { "s" }
                ),
            );
        }

        // An obligation the probes cannot reach stays in the denominator, but the
        // gap has to be declared rather than left to read as merely uncovered.
        // Only a context that actually holds an obligation counts:
        // `const MAX: usize = 10;` costs nothing and must not raise a limitation.
        let bears_obligation = |node: &ra_ap_syntax::SyntaxNode| {
            ast::StmtList::cast(node.clone()).is_some_and(|list| {
                list.statements().next().is_some() || list.tail_expr().is_some()
            }) || ast::IfExpr::can_cast(node.kind())
                || ast::WhileExpr::can_cast(node.kind())
                || ast::MatchGuard::can_cast(node.kind())
                || ast::ForExpr::can_cast(node.kind())
                || ast::MatchArm::can_cast(node.kind())
                || ast::TryExpr::can_cast(node.kind())
                || ast::ClosureExpr::can_cast(node.kind())
                || ast::BinExpr::cast(node.clone()).is_some_and(|binary| {
                    matches!(
                        binary.op_kind(),
                        Some(BinaryOp::LogicOp(LogicOp::And | LogicOp::Or))
                    )
                })
        };
        if let Some(node) = root
            .descendants()
            .find(|node| bears_obligation(node) && in_const_context(node))
        {
            self.site_limitation(
                "rust-const-context-not-instrumented",
                node.text_range(),
                "Runtime probes cannot execute in const fn or compile-time evaluation; this and any later const context in the file stay declared".into(),
            );
        }
        if let Some(node) = root
            .descendants()
            .find(|node| bears_obligation(node) && in_global_allocator(node))
        {
            self.site_limitation(
                "rust-global-allocator-not-instrumented",
                node.text_range(),
                "Probing a GlobalAlloc implementation recurses into itself, because the runtime allocates".into(),
            );
        }

        self.decline_unreachable_obligations(root);
        self.leave_out_test_code();
        self.gate_obligations();

        if let Some(error) = self.error {
            return Err(error);
        }
        self.manifest
            .decisions
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.manifest
            .points
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.manifest
            .branches
            .sort_by(|left, right| left.id.cmp(&right.id));
        self.manifest.limitations.sort_by(|left, right| {
            left.get("id")
                .and_then(|value| value.as_str())
                .cmp(&right.get("id").and_then(|value| value.as_str()))
        });
        Ok((self.manifest, self.gated))
    }
}

/// Set to a directory to receive the transformed text of any file whose
/// instrumentation no longer parses, named after the file.
pub const FAILED_TRANSFORM_DUMP_ENV: &str = "SUPERCOV_RUST_DUMP_FAILED_INSTRUMENTATION";

/// The std macros whose arguments are ordinary expressions. With the `!`
/// turned into `_` (and `vec!`'s brackets into parentheses), `name!(args)`
/// reads as the call `name_(args)` at the same byte offsets -- the same
/// statement start, the arguments an argument list. Probes then land inside
/// the arguments, and the macro receives instrumented expressions. `vec![x;
/// n]` reads as the array `[x; n]` instead, with `vec!` blanked. `matches!`
/// reads as a call on its scrutinee alone, its pattern blanked, and the whole
/// `matches!` is a decision of its own.
const EXPRESSION_MACROS: &[&str] = &[
    "assert",
    "debug_assert",
    "assert_eq",
    "assert_ne",
    "debug_assert_eq",
    "debug_assert_ne",
    "println",
    "print",
    "eprintln",
    "eprint",
    "format",
    "format_args",
    "write",
    "writeln",
    "panic",
    "unreachable",
    "todo",
    "unimplemented",
    "vec",
    "dbg",
    "matches",
];

/// Macros whose first argument decides whether the program goes on.
const ASSERTION_MACROS: &[&str] = &["assert", "debug_assert"];

/// Macros that check something and panic when it does not hold. Passing one
/// witnesses whatever ran before it.
const ASSERTION_STATEMENT_MACROS: &[&str] = &[
    "assert",
    "assert_eq",
    "assert_ne",
    "debug_assert",
    "debug_assert_eq",
    "debug_assert_ne",
];

/// The source as the instrumenter reads it: every expression macro rewritten
/// so its arguments parse as expressions, offsets intact. `assertions` holds
/// the argument-list ranges of `assert!`-like calls.
struct ExpressionView {
    text: String,
    assertions: Vec<TextRange>,
    /// The whole `assert!`-like calls, as opposed to their arguments: a
    /// statement containing one is a statement that asserts.
    assertion_calls: Vec<TextRange>,
    /// The ranges of whole `matches!(...)` calls: each is a boolean decision.
    matches: Vec<TextRange>,
}

/// The editions tried when parsing, newest first: a file that parses under
/// the newest is the common case, and an older one accepts words the newest
/// reserves -- `gen` is an identifier before 2024, and crates still call
/// `rng.gen()`. The edition that parses the original also parses its view and
/// its transformed text.
const EDITIONS: [Edition; 4] = [
    Edition::Edition2024,
    Edition::Edition2021,
    Edition::Edition2018,
    Edition::Edition2015,
];

fn parse_any_edition(source: &str) -> Result<(SourceFile, Edition), Vec<String>> {
    let mut newest_errors = None;
    for edition in EDITIONS {
        let parsed = SourceFile::parse(source, edition);
        let errors = parsed.errors();
        if errors.is_empty() {
            return Ok((parsed.tree(), edition));
        }
        newest_errors.get_or_insert_with(|| {
            errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
        });
    }
    Err(newest_errors.unwrap_or_default())
}

fn expression_view(source: &str, edition: Edition) -> ExpressionView {
    let mut text = source.to_owned();
    let mut assertions = Vec::new();
    let mut assertion_calls = Vec::new();
    let mut matches = Vec::new();
    // A macro inside another macro's arguments is tokens until the outer one
    // reads as a call, so rewrite, re-parse, and repeat until nothing changes.
    for _ in 0..16 {
        let tree = SourceFile::parse(&text, edition).tree();
        let Some(next) = rewrite_expression_macros(
            &text,
            &tree,
            edition,
            &mut assertions,
            &mut assertion_calls,
            &mut matches,
        ) else {
            break;
        };
        text = next;
    }
    ExpressionView {
        text,
        assertions,
        assertion_calls,
        matches,
    }
}

/// Whether a macro call is a statement of its own or a block's tail: there
/// its start is the statement's start, which a blanked prefix would move.
fn is_statement_macro(call: &ast::MacroCall) -> bool {
    call.syntax().parent().is_some_and(|parent| {
        ast::MacroExpr::can_cast(parent.kind())
            && parent.parent().is_some_and(|grandparent| {
                ast::ExprStmt::can_cast(grandparent.kind())
                    || ast::StmtList::can_cast(grandparent.kind())
            })
    })
}

/// The offset just after the first top-level comma of a token tree, if any.
fn first_top_level_comma_end(arguments: &ast::TokenTree) -> Option<usize> {
    arguments
        .syntax()
        .children_with_tokens()
        .filter_map(|element| element.into_token())
        .find(|token| token.kind() == SyntaxKind::COMMA)
        .map(|token| usize::from(token.text_range().end()))
}

/// One pass over the known macros of `tree`; the rewritten text, or None when
/// no macro was left to rewrite.
fn rewrite_expression_macros(
    source: &str,
    tree: &SourceFile,
    edition: Edition,
    assertions: &mut Vec<TextRange>,
    assertion_calls: &mut Vec<TextRange>,
    matches: &mut Vec<TextRange>,
) -> Option<String> {
    let mut text = source.as_bytes().to_vec();
    let mut changed = false;
    for call in tree.syntax().descendants().filter_map(ast::MacroCall::cast) {
        let Some(name) = call
            .path()
            .and_then(|path| path.segment())
            .and_then(|segment| segment.name_ref())
            .map(|name| name.text().to_string())
        else {
            continue;
        };
        if !EXPRESSION_MACROS.contains(&name.as_str()) {
            continue;
        }
        let (Some(bang), Some(arguments)) = (call.excl_token(), call.token_tree()) else {
            continue;
        };
        let parenthesised = arguments.l_paren_token().is_some();
        if !parenthesised && arguments.l_brack_token().is_none() {
            continue;
        }
        let range = arguments.syntax().text_range();
        let (start, end) = (usize::from(range.start()), usize::from(range.end()));
        let mut rewritten = source.as_bytes()[start..end].to_vec();
        if !parenthesised {
            rewritten[0] = b'(';
            *rewritten
                .last_mut()
                .expect("a token tree has a closing delimiter") = b')';
        }
        if name == "matches" {
            // `matches!(e, pat)`: the scrutinee is an expression, the pattern
            // is not. Blank the pattern so the call reads as `matches_(e, )`.
            let Some(comma_end) = first_top_level_comma_end(&arguments) else {
                continue;
            };
            blank(&mut rewritten[comma_end - start..end - start - 1]);
        }
        // The arguments must read as a call's argument list.
        let probe = format!(
            "fn __supercov() {{ let _ = __f{}; }}",
            String::from_utf8_lossy(&rewritten)
        );
        if !SourceFile::parse(&probe, edition).errors().is_empty() {
            // `vec![x; n]` is no argument list; as the array `[x; n]` it still
            // holds expressions. Blanking `vec!` moves the start of a
            // statement the macro forms on its own, so that shape stays.
            if !parenthesised && !is_statement_macro(&call) {
                let array = source.as_bytes()[start..end].to_vec();
                let array_probe = format!(
                    "fn __supercov() {{ let _ = {}; }}",
                    String::from_utf8_lossy(&array)
                );
                if SourceFile::parse(&array_probe, edition).errors().is_empty() {
                    let prefix_start = usize::from(call.syntax().text_range().start());
                    let prefix_start = call.attrs().last().map_or(prefix_start, |attribute| {
                        usize::from(attribute.syntax().text_range().end())
                    });
                    blank(&mut text[prefix_start..start]);
                    // A reference where `vec` was, so whatever the macro
                    // starts -- `vec![x; n].into()`, a comparison -- still
                    // starts there, and its probe goes before the macro
                    // rather than between `vec!` and its brackets.
                    text[prefix_start] = b'&';
                    changed = true;
                }
            }
            continue;
        }
        text[usize::from(bang.text_range().start())] = b'_';
        text[start..end].copy_from_slice(&rewritten);
        changed = true;
        if ASSERTION_MACROS.contains(&name.as_str()) {
            assertions.push(range);
        }
        if ASSERTION_STATEMENT_MACROS.contains(&name.as_str()) {
            assertion_calls.push(call.syntax().text_range());
        }
        if name == "matches" {
            matches.push(call.syntax().text_range());
        }
    }
    changed.then(|| String::from_utf8(text).expect("rewriting ASCII keeps the source UTF-8"))
}

/// How many arguments an `assert!`-like call has: one means the macro would
/// build the panic message from the condition's own text.
fn assertion_argument_count(root: &ra_ap_syntax::SyntaxNode, arguments: TextRange) -> usize {
    root.descendants()
        .find(|node| node.text_range() == arguments && ast::ArgList::can_cast(node.kind()))
        .and_then(ast::ArgList::cast)
        .map_or(0, |list| list.args().count())
}

/// The condition of an `assert!`-like call, found in the view by the range of
/// the call's argument list: its first argument.
fn assertion_condition(root: &ra_ap_syntax::SyntaxNode, arguments: TextRange) -> Option<ast::Expr> {
    root.descendants()
        .find(|node| node.text_range() == arguments && ast::ArgList::can_cast(node.kind()))
        .and_then(ast::ArgList::cast)?
        .args()
        .next()
}

/// Parse the source, then parse its expression view; the view is what the
/// collector and the instrumenter walk. Should the view not parse -- an
/// argument list that stands alone but not in place -- the original tree is
/// used and every macro stays declared.
struct ParsedSource {
    tree: SourceFile,
    assertions: Vec<TextRange>,
    assertion_calls: Vec<TextRange>,
    matches: Vec<TextRange>,
    edition: Edition,
}

fn parse_for_instrumentation(source: &str) -> Result<ParsedSource, RustInstrumenterError> {
    // The syntax tree counts in u32 offsets.
    u32::try_from(source.len()).map_err(|_| RustInstrumenterError::SourceTooLarge)?;
    let (tree, edition) = parse_any_edition(source).map_err(RustInstrumenterError::Parse)?;
    let view = expression_view(source, edition);
    let parsed_view = SourceFile::parse(&view.text, edition);
    if parsed_view.errors().is_empty() {
        Ok(ParsedSource {
            tree: parsed_view.tree(),
            assertions: view.assertions,
            assertion_calls: view.assertion_calls,
            matches: view.matches,
            edition,
        })
    } else {
        Ok(ParsedSource {
            tree,
            assertions: Vec::new(),
            assertion_calls: Vec::new(),
            matches: Vec::new(),
            edition,
        })
    }
}

/// The conditions of `if`, `while`, guards and assertions: decisions by
/// where they stand.
fn condition_expressions(
    root: &ra_ap_syntax::SyntaxNode,
    assertions: &[TextRange],
) -> Vec<ast::Expr> {
    let mut conditions = Vec::new();
    for expression in root.descendants().filter_map(ast::IfExpr::cast) {
        conditions.extend(expression.condition());
    }
    for expression in root.descendants().filter_map(ast::WhileExpr::cast) {
        conditions.extend(expression.condition());
    }
    for guard in root.descendants().filter_map(ast::MatchGuard::cast) {
        conditions.extend(guard.condition());
    }
    for arguments in assertions {
        conditions.extend(assertion_condition(root, *arguments));
    }
    conditions
}

fn is_logical(expression: &ast::Expr) -> bool {
    matches!(expression, ast::Expr::BinExpr(binary)
        if matches!(binary.op_kind(), Some(BinaryOp::LogicOp(LogicOp::And | LogicOp::Or))))
}

/// `&&` and `||` used as a value -- `let ok = a && b;`, a returned or passed
/// `x == 0 || y == 0` -- are decisions as much as a condition is: MC/DC asks
/// of every boolean expression of two or more conditions that each be shown
/// to decide it. Only the outermost operator of a tree is one, and not when
/// the tree is itself a condition, which is a decision already.
fn value_decisions(root: &ra_ap_syntax::SyntaxNode, assertions: &[TextRange]) -> Vec<ast::Expr> {
    let conditions = condition_expressions(root, assertions)
        .iter()
        .map(|condition| unparenthesized(condition).syntax().text_range())
        .collect::<Vec<_>>();
    root.descendants()
        .filter_map(ast::Expr::cast)
        .filter(is_logical)
        .filter(|expression| {
            let mut parent = expression.syntax().parent();
            while let Some(node) = parent
                .clone()
                .filter(|node| ast::ParenExpr::can_cast(node.kind()))
            {
                parent = node.parent();
            }
            !parent
                .and_then(ast::Expr::cast)
                .is_some_and(|parent| is_logical(&parent))
        })
        .filter(|expression| !conditions.contains(&expression.syntax().text_range()))
        .collect()
}

fn unparenthesized(expression: &ast::Expr) -> ast::Expr {
    match expression {
        ast::Expr::ParenExpr(paren) => paren
            .expr()
            .map_or_else(|| expression.clone(), |inner| unparenthesized(&inner)),
        _ => expression.clone(),
    }
}

/// The `matches!` calls that stand as decisions of their own: those not
/// already serving as an atomic condition of an `if`, `while`, guard or
/// assertion, whose decision records them.
fn standalone_matches(
    root: &ra_ap_syntax::SyntaxNode,
    matches: &[TextRange],
    assertions: &[TextRange],
) -> Vec<ast::Expr> {
    let mut atoms = Vec::new();
    let mut conditions = condition_expressions(root, assertions);
    conditions.extend(value_decisions(root, assertions));
    for condition in &conditions {
        RustObligationCollector::atomic_condition_ranges(condition, &mut atoms);
    }
    matches
        .iter()
        .filter(|range| !atoms.contains(range))
        .filter_map(|range| {
            root.descendants()
                .find(|node| node.text_range() == *range && ast::CallExpr::can_cast(node.kind()))
                .and_then(ast::Expr::cast)
        })
        .collect()
}

/// The cfg values of the target a build compiles for. A `#[cfg(..)]`
/// predicate over them is decided; any other key -- a feature, `test`, a
/// custom `--cfg` -- is unknown. An item is left out only when its predicate
/// is decidedly false: the compiler never builds it, so no test can reach it,
/// and the usual Rust coverage tools do not count it either.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TargetCfg {
    names: BTreeSet<String>,
    pairs: BTreeSet<(String, String)>,
    /// What one known build settles beyond the target: `test` and
    /// `debug_assertions`, and the features it enables. A Rust Worker's
    /// build is one such: worker-build compiles the crate once, never as a
    /// test, with the features its command names.
    build: BTreeMap<String, bool>,
    features: Option<BTreeSet<String>>,
}

impl TargetCfg {
    pub fn new(values: &[cargo_platform::Cfg]) -> Self {
        let mut cfg = Self::default();
        for value in values {
            match value {
                cargo_platform::Cfg::Name(name) => {
                    cfg.names.insert(name.name.clone());
                }
                cargo_platform::Cfg::KeyPair(key, value) => {
                    cfg.pairs.insert((key.name.clone(), value.clone()));
                }
            }
        }
        cfg
    }

    /// This target as one build compiles for it: `test`, `debug_assertions`
    /// and the enabled features settled too.
    pub fn for_build(
        mut self,
        test: bool,
        debug_assertions: bool,
        features: BTreeSet<String>,
    ) -> Self {
        self.build = BTreeMap::from([
            ("test".to_owned(), test),
            ("debug_assertions".to_owned(), debug_assertions),
        ]);
        self.features = Some(features);
        self
    }

    /// The keys `rustc --print cfg` settles for a target.
    fn decides(key: &str) -> bool {
        matches!(key, "unix" | "windows" | "panic") || key.starts_with("target_")
    }

    pub(crate) fn evaluate(&self, expression: &cargo_platform::CfgExpr) -> Option<bool> {
        use cargo_platform::{Cfg, CfgExpr};
        match expression {
            CfgExpr::True => Some(true),
            CfgExpr::False => Some(false),
            CfgExpr::Not(inner) => self.evaluate(inner).map(|value| !value),
            CfgExpr::All(items) => {
                let values = items
                    .iter()
                    .map(|item| self.evaluate(item))
                    .collect::<Vec<_>>();
                if values.contains(&Some(false)) {
                    Some(false)
                } else {
                    values.iter().all(Option::is_some).then_some(true)
                }
            }
            CfgExpr::Any(items) => {
                let values = items
                    .iter()
                    .map(|item| self.evaluate(item))
                    .collect::<Vec<_>>();
                if values.contains(&Some(true)) {
                    Some(true)
                } else {
                    values.iter().all(Option::is_some).then_some(false)
                }
            }
            CfgExpr::Value(Cfg::Name(name)) if Self::decides(&name.name) => {
                Some(self.names.contains(&name.name))
            }
            CfgExpr::Value(Cfg::Name(name)) => self.build.get(&name.name).copied(),
            CfgExpr::Value(Cfg::KeyPair(key, value)) if Self::decides(&key.name) => {
                Some(self.pairs.contains(&(key.name.clone(), value.clone())))
            }
            CfgExpr::Value(Cfg::KeyPair(key, value)) if key.name == "feature" => self
                .features
                .as_ref()
                .map(|features| features.contains(value)),
            CfgExpr::Value(Cfg::KeyPair(..)) => None,
        }
    }

    /// Whether a node's `#[cfg(..)]` keeps the compiler from building it.
    fn compiles_out(&self, node: &ra_ap_syntax::SyntaxNode) -> bool {
        cfg_predicate(node).is_some_and(|expression| self.evaluate(&expression) == Some(false))
    }
}

/// The `#[cfg(..)]` predicates on a node, as one: an item, a statement, a
/// match arm, a field -- anything that carries attributes, and the inner
/// `#![cfg(..)]` of a file or an inline module, which gates all of it.
fn cfg_predicate(node: &ra_ap_syntax::SyntaxNode) -> Option<cargo_platform::CfgExpr> {
    let attributes = ast::AnyHasAttrs::cast(node.clone())?;
    let mut predicates = attributes
        .attrs()
        .filter_map(|attribute| attribute.meta())
        .filter_map(|meta| {
            // Spaces inside a value are the value's: `cfg(foo = "a b")`.
            let text = meta.syntax().text().to_string();
            text.trim()
                .strip_prefix("cfg")
                .map(str::trim_start)
                .and_then(|rest| rest.strip_prefix('('))
                .and_then(|inner| inner.strip_suffix(')'))
                .and_then(|inner| inner.parse::<cargo_platform::CfgExpr>().ok())
        })
        .collect::<Vec<_>>();
    match predicates.len() {
        0 => None,
        1 => predicates.pop(),
        _ => Some(cargo_platform::CfgExpr::All(predicates)),
    }
}

/// Code behind a `#[cfg]` the target does not settle, with its predicate.
/// Without a target every predicate is unsettled.
fn unsettled_cfg_gates(
    root: &ra_ap_syntax::SyntaxNode,
    cfg: Option<&TargetCfg>,
) -> Vec<(TextRange, cargo_platform::CfgExpr)> {
    root.descendants()
        .filter_map(|node| {
            let predicate = cfg_predicate(&node)?;
            if cfg.is_some_and(|cfg| cfg.evaluate(&predicate).is_some()) {
                return None;
            }
            Some((node.text_range(), predicate))
        })
        .collect()
}

/// The outer attributes of an item, or of one inside an `impl` or a trait,
/// each as its meta text with whitespace removed: `cfg(test)`, `test`.
fn item_attributes(node: &ra_ap_syntax::SyntaxNode) -> Vec<String> {
    // Every kind an `impl` or a trait holds -- `fn`, `const`, `type`, a
    // macro call -- is an `Item` as well, so `Item` alone reads them all.
    let attributes = if let Some(item) = ast::Item::cast(node.clone()) {
        item.attrs().collect::<Vec<_>>()
    } else {
        return Vec::new();
    };
    attributes
        .iter()
        .filter_map(|attribute| attribute.meta())
        .map(|meta| {
            meta.syntax()
                .text()
                .to_string()
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect()
        })
        .collect()
}

/// Whether a file is test code by where it sits, as the usual Rust coverage
/// tools decide by default: under a `tests`, `examples` or `benches` directory, or named
/// `tests.rs`, `*_tests.rs` or `*-tests.rs`.
pub fn test_code_path(file: &str) -> bool {
    let mut parts = file.split(['/', '\\']).collect::<Vec<_>>();
    let name = parts.pop().unwrap_or_default();
    parts
        .iter()
        .any(|directory| matches!(*directory, "tests" | "examples" | "benches"))
        || name == "tests.rs"
        || name
            .strip_suffix("tests.rs")
            .and_then(|stem| stem.strip_suffix(['_', '-']))
            .is_some_and(|stem| {
                !stem.is_empty()
                    && stem.chars().all(|character| {
                        character.is_ascii_alphanumeric() || "_-".contains(character)
                    })
            })
}

/// Whether an item is test code: gated on `cfg(test)`, alone or first in an
/// `all(..)`, or a test or benchmark function (`#[test]`, `#[tokio::test]`).
pub(crate) fn test_item(node: &ra_ap_syntax::SyntaxNode) -> bool {
    item_attributes(node).iter().any(|text| {
        let path = text
            .chars()
            .take_while(|character| character.is_alphanumeric() || "_:".contains(*character))
            .collect::<String>();
        matches!(path.rsplit("::").next(), Some("test" | "bench"))
            || text == "cfg(test)"
            || text.starts_with("cfg(all(test,")
            || text.starts_with("cfg(all(test)")
    })
}

/// The items the target's cfg leaves uncompiled.
fn compiled_out_ranges(root: &ra_ap_syntax::SyntaxNode, cfg: Option<&TargetCfg>) -> Vec<TextRange> {
    let Some(cfg) = cfg else {
        return Vec::new();
    };
    root.descendants()
        .filter(|node| cfg.compiles_out(node))
        .map(|node| node.text_range())
        .collect()
}

/// The test code in a file: all of it, or its test items.
fn test_code_ranges(root: &ra_ap_syntax::SyntaxNode, test_file: bool) -> Vec<TextRange> {
    if test_file {
        return vec![root.text_range()];
    }
    root.descendants()
        .filter(test_item)
        .map(|node| node.text_range())
        .collect()
}

fn in_test_code(tests: &[TextRange], range: TextRange) -> bool {
    tests.iter().any(|test| test.contains_range(range))
}

pub fn build_rust_manifest(
    file: &str,
    source: &str,
) -> Result<CoverageManifest, RustInstrumenterError> {
    let parsed = parse_for_instrumentation(source)?;
    let tests = test_code_ranges(parsed.tree.syntax(), test_code_path(file));
    RustObligationCollector::new(file, source)
        .leaving_out(tests, Vec::new())
        .collect(&parsed.tree, &parsed.assertions, &parsed.matches)
}

/// Where the probe of a statement that starts at `offset` goes.
///
/// Rust reports a panic by line and column, `src/lib.rs:8:9`. A probe put
/// straight before a statement moved it along its line, to `:8:84`, and one
/// put on a line of its own moved every line below. A statement that begins
/// its line has only blank space between it and the token before it, and
/// anywhere in that space is the same place in the program: its probe goes
/// to the far end, after that token, and the statement stays where it was
/// written. One that shares its line with code before it has no such room,
/// and its probe stays in front of it.
fn before_its_line(root: &ra_ap_syntax::SyntaxNode, source: &str, offset: usize) -> usize {
    let line = source[..offset].rfind('\n').map_or(0, |at| at + 1);
    if line == 0 || !source[line..offset].chars().all(char::is_whitespace) {
        return offset;
    }
    let Ok(at) = u32::try_from(offset) else {
        return offset;
    };
    let mut before = root
        .token_at_offset(ra_ap_syntax::TextSize::from(at))
        .right_biased()
        .and_then(|token| token.prev_token());
    while let Some(token) = &before {
        if !token.kind().is_trivia() {
            return usize::from(token.text_range().end());
        }
        before = token.prev_token();
    }
    offset
}

fn block_entry_offset(block: &ast::BlockExpr) -> Option<usize> {
    let list = block.stmt_list()?;
    list.attrs()
        .last()
        .map(|attribute| usize::from(attribute.syntax().text_range().end()))
        .or_else(|| {
            list.l_curly_token()
                .map(|token| usize::from(token.text_range().end()))
        })
}

/// `block_entry_offset`, for a block of a tree that parsed. The parser opens
/// every block at its `{` but a `try` written without one, which is a parse
/// error and so never instrumented; were one reached, its probe would have
/// nowhere to go, and the file is refused rather than probed wrongly.
/// A test fault (`rust-block-entry`) stands in for such a block, so each
/// place that would refuse the file runs under test.
fn block_entry(block: &ast::BlockExpr) -> Result<usize, RustInstrumenterError> {
    crate::fault::or("rust-block-entry", || None, || block_entry_offset(block))
        .ok_or(RustInstrumenterError::InvalidRange)
}

/// A node's range without its outer attributes: a wrapper placed here stays
/// under the attributes, so `#[cfg]` governs the wrapper and the node alike.
/// A node's range from its first real token: past the doc comments and
/// attributes that lead it, which the syntax tree folds into the node.
fn item_range(node: &ra_ap_syntax::SyntaxNode) -> TextRange {
    let range = node.text_range();
    node.children_with_tokens()
        .find(|element| match element {
            ra_ap_syntax::NodeOrToken::Token(token) => !token.kind().is_trivia(),
            ra_ap_syntax::NodeOrToken::Node(child) => !ast::Attr::can_cast(child.kind()),
        })
        .map_or(range, |element| {
            TextRange::new(element.text_range().start(), range.end())
        })
}

fn range_after_attributes(node: &impl HasAttrs) -> TextRange {
    let range = node.syntax().text_range();
    node.attrs().last().map_or(range, |attribute| {
        TextRange::new(attribute.syntax().text_range().end(), range.end())
    })
}

fn has_let(expression: &ast::Expr) -> bool {
    expression
        .syntax()
        .descendants()
        .any(|node| ast::LetExpr::can_cast(node.kind()))
}

/// The expression whose condition is a let chain.
enum ChainHost<'a> {
    If(&'a ast::IfExpr),
    While(&'a ast::WhileExpr),
}

/// The `const` naming a let chain's `&&` operators and their alternative IDs.
fn allocate_chain_table_name(
    file: &str,
    condition: &ast::Expr,
    identifiers: &mut BTreeSet<String>,
) -> String {
    let id = stable_id(file, "chain", condition.syntax().text_range(), "operators");
    let suffix = id
        .rsplit(':')
        .next()
        .unwrap_or("chain")
        .to_ascii_uppercase();
    let base = format!("__SUPERCOV_CHAIN_{suffix}");
    let mut candidate = base.clone();
    let mut attempt = 0_usize;
    while !identifiers.insert(candidate.clone()) {
        attempt += 1;
        candidate = format!("{base}_{attempt}");
    }
    candidate
}

/// A fresh identifier for generated code, from the obligation it serves.
fn allocate_identifier(
    file: &str,
    range: TextRange,
    kind: &str,
    identifiers: &mut BTreeSet<String>,
) -> String {
    let id = stable_id(file, kind, range, "");
    let suffix = id.rsplit(':').next().unwrap_or(kind);
    let base = format!("__supercov_{kind}_{suffix}");
    let mut candidate = base.clone();
    let mut attempt = 0_usize;
    while !identifiers.insert(candidate.clone()) {
        attempt += 1;
        candidate = format!("{base}_{attempt}");
    }
    candidate
}

/// The `break`s in `body` that leave the loop it belongs to: unlabeled ones
/// with no other loop between them and the body, and labeled ones naming the
/// loop's own label.
fn own_breaks(body: &ast::BlockExpr, label: Option<ast::Label>) -> Vec<TextRange> {
    let own_label = label
        .and_then(|label| label.lifetime())
        .map(|lifetime| lifetime.text().to_string());
    body.syntax()
        .descendants()
        .filter_map(ast::BreakExpr::cast)
        .filter(|expression| match expression.lifetime() {
            Some(lifetime) => own_label.as_deref() == Some(lifetime.text().to_string().as_str()),
            None => !expression
                .syntax()
                .ancestors()
                .skip(1)
                .take_while(|ancestor| ancestor != body.syntax())
                .any(|ancestor| {
                    ast::LoopExpr::can_cast(ancestor.kind())
                        || ast::WhileExpr::can_cast(ancestor.kind())
                        || ast::ForExpr::can_cast(ancestor.kind())
                        || ast::ClosureExpr::can_cast(ancestor.kind())
                }),
        })
        .map(|expression| expression.syntax().text_range())
        .collect()
}

/// A decision whose condition holds a `let`. A `let` cannot pass through a
/// call and the condition cannot be wrapped as a whole, so the frame lives in
/// a block around the `if` or `while`, ordinary conditions take `condition`
/// wrappers, each later `let` of a chain is preceded by a `reached` marker,
/// and the outcome is recorded where it becomes known: at the entry of the
/// then branch or loop body (taken) and at the else branch or after the loop
/// (not taken). From those, the runtime derives every pattern's outcome
/// exactly: a chain tries its conditions in order and stops at the first that
/// fails. The chain's `&&` operators are recorded from the same frame, through
/// a table of the operators whose left side holds a `let`.
///
/// A lone `let` is not a chain, and an `&&` marker would make it one -- which
/// editions before 2024 reject. Its evaluation is marked by a statement
/// instead: once before an `if`, and for a `while` at each body entry and
/// after the loop, where a `break` has to be told from the condition failing.
fn instrument_let_chain(
    insertions: &mut Vec<Insertion>,
    runtime_path: &str,
    file: &str,
    condition: &ast::Expr,
    host: ChainHost<'_>,
    identifiers: &mut BTreeSet<String>,
) -> Result<(), RustInstrumenterError> {
    // The block goes after any outer attributes, so `#[cfg]` keeps governing
    // the frame together with the expression it belongs to.
    let (kind, host_range, body, label) = match &host {
        ChainHost::If(expression) => (
            "if",
            range_after_attributes(*expression),
            expression.then_branch(),
            None,
        ),
        ChainHost::While(expression) => (
            "while",
            range_after_attributes(*expression),
            expression.loop_body(),
            expression.label(),
        ),
    };
    let Some(body) = body else {
        return Ok(());
    };
    let body_offset = block_entry(&body)?;
    let range = condition.syntax().text_range();
    let id = stable_id(file, "decision", range, kind);
    let mut atoms = Vec::new();
    RustObligationCollector::atomic_condition_ranges(condition, &mut atoms);
    let lets = condition
        .syntax()
        .descendants()
        .filter_map(ast::LetExpr::cast)
        .map(|expression| expression.syntax().text_range())
        .collect::<Vec<_>>();
    let frame = allocate_frame_name(file, condition, kind, identifiers);
    let table = allocate_chain_table_name(file, condition, identifiers);
    let single_let = atoms.len() == 1;
    let broke = match &host {
        ChainHost::While(_) if single_let => {
            Some(allocate_identifier(file, range, "broke", identifiers))
        }
        _ => None,
    };

    let mut operators = Vec::new();
    for binary in condition
        .syntax()
        .descendants()
        .filter_map(ast::BinExpr::cast)
    {
        // Let chains are `&&`-only at the top level; an operator whose left
        // side holds a `let` is one the logical wrapper could not touch.
        if !matches!(binary.op_kind(), Some(BinaryOp::LogicOp(LogicOp::And))) {
            continue;
        }
        let (left, right) = (
            binary
                .lhs()
                .expect("a tree parsed without errors has every part its syntax requires"),
            binary
                .rhs()
                .expect("a tree parsed without errors has every part its syntax requires"),
        );
        if !has_let(&left) {
            continue;
        }
        let branch = stable_id(file, "branch", right.syntax().text_range(), "logical-and");
        let right_range = right.syntax().text_range();
        let Some(first) = atoms
            .iter()
            .position(|atom| right_range.contains_range(*atom))
        else {
            continue;
        };
        operators.push(format!(
            "({first}, {}, {})",
            hit_ordinal(&format!("{branch}:short-circuit")),
            hit_ordinal(&format!("{branch}:evaluated"))
        ));
    }

    let mark = format!("{runtime_path}::reached(&mut {frame}, 0);");
    let record_false = format!("{runtime_path}::decision_chain(&mut {frame}, false, {table});");
    let mut prefix = format!(
        "{{ const {table}: &[(usize, u32, u32)] = &[{}]; let mut {frame} = {runtime_path}::DecisionFrame::new({}, {}); ",
        operators.join(", "),
        decision_ordinal(&id),
        atoms.len()
    );
    if single_let {
        prefix.push_str(&mark);
        prefix.push(' ');
    }
    if let Some(broke) = &broke {
        prefix.push_str(&format!("let mut {broke} = false; "));
    }
    let suffix = match &host {
        ChainHost::If(expression) => match expression.else_branch() {
            Some(_) => " }".to_owned(),
            None => format!(" else {{ {record_false} }} }}"),
        },
        ChainHost::While(_) => match &broke {
            Some(broke) => format!(" if !{broke} {{ {mark} {record_false} }} }}"),
            None => format!(" {record_false} }}"),
        },
    };
    push_wrapper(insertions, host_range, host_range, 1, prefix, suffix);
    if !single_let {
        push_direct(
            insertions,
            usize::from(range.start()),
            format!("{runtime_path}::reached(&mut {frame}, 0) && "),
        );
    }
    // A chain holds at least one condition.
    atoms.iter().enumerate().for_each(|(index, atom)| {
        if lets.contains(atom) {
            if index > 0 {
                push_direct(
                    insertions,
                    usize::from(atom.start()),
                    format!("{runtime_path}::reached(&mut {frame}, {index}) && "),
                );
            }
        } else {
            push_wrapper(
                insertions,
                *atom,
                *atom,
                1,
                format!("{runtime_path}::condition(("),
                format!("), &mut {frame}, {index})"),
            );
        }
    });
    let mut entry = String::new();
    if broke.is_some() {
        entry.push_str(&format!(" {mark}"));
    }
    entry.push_str(&format!(
        " {runtime_path}::decision_chain(&mut {frame}, true, {table});"
    ));
    push_direct(insertions, body_offset, entry);
    if let Some(broke) = &broke {
        for break_range in own_breaks(&body, label) {
            push_wrapper(
                insertions,
                break_range,
                break_range,
                0,
                format!("{{ {broke} = true; "),
                " }".into(),
            );
        }
    }
    if let ChainHost::If(expression) = &host {
        match expression.else_branch() {
            Some(ast::ElseBranch::Block(block)) => {
                push_direct(insertions, block_entry(&block)?, format!(" {record_false}"));
            }
            Some(ast::ElseBranch::IfExpr(nested)) => {
                let nested_range = nested.syntax().text_range();
                push_wrapper(
                    insertions,
                    nested_range,
                    nested_range,
                    0,
                    format!("{{ {record_false} "),
                    " }".into(),
                );
            }
            None => {}
        }
    }
    Ok(())
}

/// Where an item for `node` can go: the entry of the nearest enclosing block.
/// An item declared there is visible to the whole block, so nothing about the
/// expression itself -- its value, its temporaries -- changes.
fn enclosing_block_entry(node: &ra_ap_syntax::SyntaxNode) -> Option<usize> {
    node.ancestors()
        .skip(1)
        .find_map(ast::BlockExpr::cast)
        .and_then(|block| block_entry_offset(&block))
}

/// A block written as a bare `{ ... }`: a probe placed just inside its brace
/// runs when the block is entered. Labeled, `unsafe`, `async` and `const`
/// blocks are wrapped instead, so an `async` body does not defer the probe.
fn plain_block(block: &ast::BlockExpr) -> bool {
    block
        .syntax()
        .first_token()
        .is_some_and(|token| token.kind() == SyntaxKind::L_CURLY)
}

fn instrument_decision(
    insertions: &mut Vec<Insertion>,
    runtime_path: &str,
    file: &str,
    condition: &ast::Expr,
    kind: &str,
    frame_name: &str,
) -> bool {
    if cannot_carry_probe(condition.syntax())
        || condition
            .syntax()
            .descendants()
            .any(|node| ast::LetExpr::can_cast(node.kind()))
    {
        return false;
    }
    let range = condition.syntax().text_range();
    let id = stable_id(file, "decision", range, kind);
    let mut condition_ranges = Vec::new();
    RustObligationCollector::atomic_condition_ranges(condition, &mut condition_ranges);
    push_wrapper(
        insertions,
        range,
        range,
        0,
        format!(
            "({{ let mut {frame_name} = {runtime_path}::DecisionFrame::new({}, {}); {runtime_path}::decision((",
            decision_ordinal(&id),
            condition_ranges.len()
        ),
        format!("), &mut {frame_name}) }})"),
    );
    // Each condition's scope is the condition itself, so a wrapper that
    // started earlier and ends where this condition ends -- the left operand
    // of a logical operator -- closes after it, not before.
    // A decision has a condition.
    condition_ranges
        .into_iter()
        .enumerate()
        .for_each(|(index, atomic_range)| {
            push_wrapper(
                insertions,
                atomic_range,
                atomic_range,
                1,
                format!("{runtime_path}::condition(("),
                format!("), &mut {frame_name}, {index})"),
            );
        });
    true
}

/// Produce a private Rust candidate using only Supercov-owned probe calls.
///
/// The caller supplies a collision-free generated crate-local runtime path.
/// Every obligation the manifest declares -- statements, functions, decisions
/// with their conditions -- let chains included -- match arms, logical
/// operators, loops and the try operator -- takes an owned probe; what a
/// probe cannot reach (const contexts, macro expansions, an attributed `let`
/// with no initializer) stays in the denominator behind an explicit
/// limitation.
pub fn instrument_rust_source(
    file: &str,
    source: &str,
    runtime_path: &str,
) -> Result<RustInstrumentedSource, RustInstrumenterError> {
    instrument_rust_file(file, source, runtime_path, test_code_path(file), None)
}

/// `instrument_rust_source` for a file whose test status is known from how
/// the crate reaches it: a module only `#[cfg(test)]` declares is test code
/// whatever its name. Test code keeps only its assertion markers; an item
/// `cfg` leaves uncompiled for the target keeps nothing.
pub fn instrument_rust_file(
    file: &str,
    source: &str,
    runtime_path: &str,
    test_file: bool,
    cfg: Option<&TargetCfg>,
) -> Result<RustInstrumentedSource, RustInstrumenterError> {
    let mut layout = RustProbeLayout::default();
    let mut transformed =
        instrument_rust_file_numbered(file, source, runtime_path, test_file, cfg, &mut layout)?;
    transformed.probes = layout;
    Ok(transformed)
}

/// `instrument_rust_file`, numbering its probes in `layout`, which carries
/// on from the files instrumented before it: one numbering spans the
/// workspace, so every crate's runtime has the same slots.
pub fn instrument_rust_file_numbered(
    file: &str,
    source: &str,
    runtime_path: &str,
    test_file: bool,
    cfg: Option<&TargetCfg>,
    layout: &mut RustProbeLayout,
) -> Result<RustInstrumentedSource, RustInstrumenterError> {
    // One file's bug costs that file its measurement, not the run.
    crate::contained::contained(|| {
        crate::fault::or(
            "instrument-panic",
            || panic!("injected fault at instrument-panic"),
            || numbered_file(file, source, runtime_path, test_file, cfg, layout),
        )
    })
    .unwrap_or_else(|message| Err(RustInstrumenterError::Internal(message)))
}

fn numbered_file(
    file: &str,
    source: &str,
    runtime_path: &str,
    test_file: bool,
    cfg: Option<&TargetCfg>,
    layout: &mut RustProbeLayout,
) -> Result<RustInstrumentedSource, RustInstrumenterError> {
    let _numbering = NumberingScope::enter(layout);
    if !valid_runtime_path(runtime_path) {
        return Err(RustInstrumenterError::InvalidRuntimePath);
    }
    // Parsing is the whole cost of preparing a workspace -- 33s of regex's
    // 34s, 18s of tokio's 19s -- and every file was parsed twice: once to
    // build the manifest and once to place the probes. Each parse tries the
    // editions in turn and then rewrites the expression view to a fixpoint,
    // re-parsing each round.
    let parsed = parse_for_instrumentation(source)?;
    let tests = test_code_ranges(parsed.tree.syntax(), test_file);
    let compiled_out = compiled_out_ranges(parsed.tree.syntax(), cfg);
    // Code behind a predicate only the build can settle: its obligations are
    // counted unless every build that ran leaves it out (`RustCfgGate`).
    let gates = unsettled_cfg_gates(parsed.tree.syntax(), cfg)
        .into_iter()
        .filter(|(range, _)| !in_test_code(&tests, *range) && !in_test_code(&compiled_out, *range))
        .collect::<Vec<_>>();
    let (mut manifest, gated) = RustObligationCollector::new(file, source)
        .leaving_out(tests.clone(), compiled_out.clone())
        .gating(gates.iter().map(|(range, _)| *range).collect())
        .collect_gated(&parsed.tree, &parsed.assertions, &parsed.matches)?;
    for ((range, predicate), obligations) in gates.iter().zip(gated) {
        if obligations.is_empty() {
            continue;
        }
        let line_of = |offset: ra_ap_syntax::TextSize| {
            source[..usize::from(offset)].matches('\n').count() + 1
        };
        cfg_gate_ordinal(RustCfgGate {
            file: file.to_owned(),
            predicate: predicate.to_string(),
            line: line_of(range.start()),
            end_line: line_of(range.end()),
            obligations,
        });
    }
    let ParsedSource {
        tree,
        assertions,
        assertion_calls,
        matches,
        edition,
    } = parsed;
    let root = tree.syntax();
    let mut insertions = Vec::new();
    let mut identifiers = root
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| token.kind() == SyntaxKind::IDENT)
        .map(|token| token.text().to_string())
        .collect::<BTreeSet<_>>();

    let mut skipped_attributed_statement = false;
    // A probe must never be PREPENDED to a statement that carries outer
    // attributes. `#[cfg]` selects among adjacent statements, and a bare
    // `hit(...)` inserted between them survives the strip and changes which
    // expression is the block's tail: memchr's `is_available` returns bool
    // from one of two cfg-gated blocks, and the stray probe turned the kept
    // block into a statement and the probe itself into a `()` tail -- 32
    // E0308s across the crate. An attributed BLOCK takes the probe inside its
    // braces, where the same cfg governs both; any other attributed
    // expression is wrapped in such a block. Only a `let` with no initializer
    // has nowhere to put a probe, and is declared.
    let attributed_probe = |insertions: &mut Vec<Insertion>,
                            skipped: &mut bool,
                            expression: Option<ast::Expr>,
                            has_attrs: bool,
                            // Whether `expression` IS the statement, as
                            // opposed to the initializer of a `let`, whose
                            // value the binding needs.
                            statement: bool,
                            trailing: bool,
                            range: TextRange,
                            id: String|
     -> Result<(), RustInstrumenterError> {
        if !has_attrs {
            push_direct(
                insertions,
                before_its_line(root, source, usize::from(range.start())),
                format!("{runtime_path}::hit({});", hit_ordinal(&id)),
            );
            return Ok(());
        }
        let Some(expression) = expression else {
            *skipped = true;
            return Ok(());
        };
        if let ast::Expr::BlockExpr(block) = &expression {
            push_direct(
                insertions,
                block_entry(block)?,
                format!("{runtime_path}::hit({});", hit_ordinal(&id)),
            );
            return Ok(());
        }
        // A brace-delimited macro call closing a block without a semicolon
        // is a STATEMENT to rustc even though it supplies the block's value
        // -- `fn f() -> T { #[rustfmt::skip] m! { .. } }` compiles -- and
        // wrapping it as `#[attr] { hit; (m! { .. }) }` would make it an
        // attributed tail expression, which is unstable (E0658; tokio's
        // `#[rustfmt::skip] tokio::select! { .. }`). The probe goes before
        // the attributes instead, in a block wrapping attributes and macro
        // together, where the macro is again a trailing statement. A `cfg`
        // attribute would then fire the probe for a statement cfg strips, so
        // that shape is declared.
        if trailing && let ast::Expr::MacroExpr(_) = &expression {
            if expression.attrs().any(|attribute| {
                // This grammar parses `cfg(..)` as a keyword and predicate
                // rather than a path, so the meta's leading word is read.
                attribute.meta().is_some_and(|meta| {
                    let text = meta.syntax().text().to_string();
                    let name = text
                        .chars()
                        .take_while(|character| character.is_alphanumeric() || *character == '_')
                        .collect::<String>();
                    matches!(name.as_str(), "cfg" | "cfg_attr")
                })
            }) {
                *skipped = true;
                return Ok(());
            }
            let range = expression.syntax().text_range();
            push_wrapper(
                insertions,
                range,
                range,
                0,
                format!("{{ {runtime_path}::hit({}); ", hit_ordinal(&id)),
                " }".into(),
            );
            return Ok(());
        }
        // An attributed macro STATEMENT keeps its macro in statement
        // position: what a macro expands to may only be legal there.
        // hyper's `trace!` expands to `#[cfg(feature = "tracing")] { .. }`,
        // and `#[cfg(..)] { hit; (trace!("..")) }` makes that expansion an
        // attributed expression (E0658). The probe goes ahead of it inside
        // the block instead, and the attribute still governs both.
        if statement && let ast::Expr::MacroExpr(_) = &expression {
            let start = expression.attrs().last().map_or_else(
                || expression.syntax().text_range().start(),
                |attribute| attribute.syntax().text_range().end(),
            );
            let wrapped = TextRange::new(start, expression.syntax().text_range().end());
            push_wrapper(
                insertions,
                wrapped,
                wrapped,
                0,
                format!(" {{ {runtime_path}::hit({}); ", hit_ordinal(&id)),
                "; }".into(),
            );
            return Ok(());
        }
        // Any other attributed expression -- or the initializer of an
        // attributed `let` -- moves into a block that carries the probe: the
        // attributes now govern probe and expression together, the block has
        // the expression's value where the expression was, and a block's tail
        // still extends the temporaries a `let` would have extended.
        let start = expression.attrs().last().map_or_else(
            || expression.syntax().text_range().start(),
            |attribute| attribute.syntax().text_range().end(),
        );
        let wrapped = TextRange::new(start, expression.syntax().text_range().end());
        push_wrapper(
            insertions,
            wrapped,
            wrapped,
            0,
            format!(" {{ {runtime_path}::hit({}); (", hit_ordinal(&id)),
            ") }".into(),
        );
        Ok(())
    };
    for list in root.descendants().filter_map(ast::StmtList::cast) {
        for statement in list.statements() {
            // No statement closes its block: the grammar makes an expression
            // that does, without a semicolon before the `}`, the block's tail,
            // which is probed below.
            let (range, expression, has_attrs, statement_expression) = match &statement {
                ast::Stmt::ExprStmt(statement) if !cannot_carry_probe(statement.syntax()) => {
                    let expression = statement.expr();
                    // Outer attributes on an expression statement attach to
                    // the inner expression in this grammar.
                    let has_attrs = expression
                        .as_ref()
                        .is_some_and(|expression| expression.attrs().next().is_some());
                    (statement.syntax().text_range(), expression, has_attrs, true)
                }
                ast::Stmt::LetStmt(statement) if !cannot_carry_probe(statement.syntax()) => {
                    let has_attrs = statement.attrs().next().is_some();
                    // Only an attributed `let` needs its initializer; a plain
                    // one takes the probe before the statement.
                    let initializer = has_attrs.then(|| statement.initializer()).flatten();
                    (
                        statement.syntax().text_range(),
                        initializer,
                        has_attrs,
                        false,
                    )
                }
                _ => continue,
            };
            let id = stable_id(file, "statement", range, "");
            // Passing a statement that asserts means every assertion in it
            // held: a failing one panics instead. What this thread recorded
            // before that point was in scope for the check. An attributed
            // statement gets no marker -- a `cfg` that strips the statement
            // would leave the marker behind -- and neither does a block's
            // tail, where a statement after it would change the value.
            if !has_attrs
                && assertion_calls
                    .iter()
                    .any(|call| range.contains_range(*call))
            {
                push_direct(
                    &mut insertions,
                    usize::from(range.end()),
                    format!("{runtime_path}::assertion({});", assertion_ordinal(&id)),
                );
            }
            attributed_probe(
                &mut insertions,
                &mut skipped_attributed_statement,
                expression,
                has_attrs,
                statement_expression,
                false,
                range,
                id,
            )?;
        }
        if let Some(tail) = list
            .tail_expr()
            .filter(|tail| !cannot_carry_probe(tail.syntax()))
        {
            let range = tail.syntax().text_range();
            let id = stable_id(file, "statement", range, "");
            let has_attrs = tail.attrs().next().is_some();
            attributed_probe(
                &mut insertions,
                &mut skipped_attributed_statement,
                Some(tail),
                has_attrs,
                true,
                true,
                range,
                id,
            )?;
        }
    }

    for function in root.descendants().filter_map(ast::Fn::cast) {
        // `cannot_carry_probe` covers `const fn` itself, since a node's own
        // ancestors include the node.
        if cannot_carry_probe(function.syntax()) {
            continue;
        }
        let Some(body) = function.body() else {
            continue;
        };
        let label = function.name().map(|name| name.text().to_string());
        let id = stable_id(
            file,
            "function",
            function.syntax().text_range(),
            label.as_deref().unwrap_or(""),
        );
        push_direct(
            &mut insertions,
            block_entry(&body)?,
            format!("{runtime_path}::hit({});", hit_ordinal(&id)),
        );
    }

    for closure in root.descendants().filter_map(ast::ClosureExpr::cast) {
        let body = closure
            .body()
            .expect("a tree parsed without errors has every part its syntax requires");
        if cannot_carry_probe(body.syntax()) {
            continue;
        }
        let id = stable_id(file, "function", closure.syntax().text_range(), "<closure>");
        if let ast::Expr::BlockExpr(block) = &body {
            push_direct(
                &mut insertions,
                block_entry(block)?,
                format!("{runtime_path}::hit({});", hit_ordinal(&id)),
            );
        } else {
            let range = body.syntax().text_range();
            push_wrapper(
                &mut insertions,
                range,
                closure.syntax().text_range(),
                0,
                format!("{{ {runtime_path}::hit({}); (", hit_ordinal(&id)),
                ") }".into(),
            );
        }
    }

    // Match arms. One `const` per match, at the entry of the enclosing block,
    // names every arm's `not selected` and `selected` IDs in source order;
    // each arm then records itself as selected and every arm before it as
    // passed over, since a match tries its arms in order and stops at the
    // first that fits. The runtime dedupes by ID, so a hot match costs one
    // record per alternative.
    for expression in root.descendants().filter_map(ast::MatchExpr::cast) {
        if cannot_carry_probe(expression.syntax()) {
            continue;
        }
        let list = expression
            .match_arm_list()
            .expect("a tree parsed without errors has every part its syntax requires");
        let arms = list.arms().collect::<Vec<_>>();
        if arms.is_empty() {
            continue;
        }
        let Some(table_offset) = enclosing_block_entry(expression.syntax()) else {
            continue;
        };
        let table = allocate_table_name(file, &expression, &mut identifiers);
        let entries = arms
            .iter()
            .map(|arm| {
                let id = stable_id(file, "branch", arm.syntax().text_range(), "match-arm");
                format!(
                    "{}, {}",
                    hit_ordinal(&format!("{id}:missed")),
                    hit_ordinal(&format!("{id}:selected"))
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        push_direct(
            &mut insertions,
            table_offset,
            // On the brace's line, as every probe is: a line of its own
            // moved each line below it, and with them where a panic or a
            // doctest further down the file is reported.
            format!(" const {table}: &[u32] = &[{entries}];"),
        );
        for (index, arm) in arms.iter().enumerate() {
            let body = arm
                .expr()
                .expect("a tree parsed without errors has every part its syntax requires");
            let call = format!("{runtime_path}::arms({table}, {index});");
            match &body {
                ast::Expr::BlockExpr(block) if plain_block(block) => {
                    push_direct(&mut insertions, block_entry(block)?, format!(" {call}"));
                }
                _ => push_wrapper(
                    &mut insertions,
                    body.syntax().text_range(),
                    arm.syntax().text_range(),
                    0,
                    format!("{{ {call} ("),
                    ") }".into(),
                ),
            }
        }
    }

    // Logical operators: the left operand alone decides whether the right one
    // runs, so wrapping it records the outcome without touching evaluation
    // order. `&&` short-circuits on false, `||` on true.
    for binary in root.descendants().filter_map(ast::BinExpr::cast) {
        let short_circuits_when = match binary.op_kind() {
            Some(BinaryOp::LogicOp(LogicOp::And)) => false,
            Some(BinaryOp::LogicOp(LogicOp::Or)) => true,
            _ => continue,
        };
        if cannot_carry_probe(binary.syntax()) {
            continue;
        }
        let (left, right) = (
            binary
                .lhs()
                .expect("a tree parsed without errors has every part its syntax requires"),
            binary
                .rhs()
                .expect("a tree parsed without errors has every part its syntax requires"),
        );
        // In a let chain the left operand is (or holds) a `let`, whose
        // bindings must stay in scope for the right operand; it cannot pass
        // through a call. The chain's own probes record that operator.
        if has_let(&left) {
            continue;
        }
        let kind = if short_circuits_when {
            "logical-or"
        } else {
            "logical-and"
        };
        let id = stable_id(file, "branch", right.syntax().text_range(), kind);
        push_wrapper(
            &mut insertions,
            left.syntax().text_range(),
            binary.syntax().text_range(),
            2,
            format!("{runtime_path}::logical(("),
            format!(
                "), {short_circuits_when}, {}, {})",
                hit_ordinal(&format!("{id}:short-circuit")),
                hit_ordinal(&format!("{id}:evaluated"))
            ),
        );
    }

    // `for` loops: the iterable passes through an adapter that records, on
    // the first `next`, whether the body ran at all. `into_iter` is called
    // where the loop would have called it, on the same expression.
    for expression in root.descendants().filter_map(ast::ForExpr::cast) {
        if cannot_carry_probe(expression.syntax()) {
            continue;
        }
        let iterable = expression
            .iterable()
            .expect("a tree parsed without errors has every part its syntax requires");
        let id = stable_id(file, "branch", expression.syntax().text_range(), "for-loop");
        // A loop that cannot be empty has no zero-iterations outcome: the
        // adapter is given its entry ordinal for both, and the first is
        // never reported.
        let entered = hit_ordinal(&format!("{id}:entered"));
        let zero = if never_empty(&iterable) {
            entered
        } else {
            hit_ordinal(&format!("{id}:zero"))
        };
        // Scope is the wrapped range itself: any wrapper that also starts
        // here and reaches further -- a decision, a match arm -- must stay
        // outside this one.
        push_wrapper(
            &mut insertions,
            iterable.syntax().text_range(),
            iterable.syntax().text_range(),
            0,
            format!("{runtime_path}::for_loop(("),
            format!("), {zero}, {entered})"),
        );
    }

    // `while` loops: a flag beside the loop, cleared by the first body entry
    // and read once the loop is over. The condition stays as written, so
    // `while let` is covered too.
    for expression in root.descendants().filter_map(ast::WhileExpr::cast) {
        if cannot_carry_probe(expression.syntax()) {
            continue;
        }
        let Some(offset) = expression.loop_body().as_ref().and_then(block_entry_offset) else {
            continue;
        };
        let id = stable_id(
            file,
            "branch",
            expression.syntax().text_range(),
            "while-loop",
        );
        let flag = allocate_flag_name(file, &expression, &mut identifiers);
        let range = range_after_attributes(&expression);
        push_wrapper(
            &mut insertions,
            range,
            range,
            0,
            format!("{{ let mut {flag} = true; "),
            format!(
                " {runtime_path}::zero_iterations({flag}, {}) }}",
                hit_ordinal(&format!("{id}:zero"))
            ),
        );
        push_direct(
            &mut insertions,
            offset,
            format!(
                " {runtime_path}::entered(&mut {flag}, {});",
                hit_ordinal(&format!("{id}:entered"))
            ),
        );
    }

    // The try operator: the operand passes through a probe that reads which
    // way `?` will go. Every stable `Try` type is covered by the runtime's
    // `TryProbe` implementations.
    for expression in root.descendants().filter_map(ast::TryExpr::cast) {
        if cannot_carry_probe(expression.syntax()) {
            continue;
        }
        let operand = expression
            .expr()
            .expect("a tree parsed without errors has every part its syntax requires");
        let id = stable_id(
            file,
            "branch",
            expression.syntax().text_range(),
            "try-operator",
        );
        // Scope is the operand alone: a decision wrapping `expr?` as its
        // condition starts at the same offset and must close after this.
        push_wrapper(
            &mut insertions,
            operand.syntax().text_range(),
            operand.syntax().text_range(),
            0,
            format!("{runtime_path}::TryProbe::probe(("),
            format!(
                "), {}, {})",
                hit_ordinal(&format!("{id}:continued")),
                hit_ordinal(&format!("{id}:returned"))
            ),
        );
    }

    for expression in root.descendants().filter_map(ast::IfExpr::cast) {
        let condition = expression
            .condition()
            .expect("a tree parsed without errors has every part its syntax requires");
        if has_let(&condition) {
            if !cannot_carry_probe(condition.syntax()) {
                instrument_let_chain(
                    &mut insertions,
                    runtime_path,
                    file,
                    &condition,
                    ChainHost::If(&expression),
                    &mut identifiers,
                )?;
            }
            continue;
        }
        let frame_name = allocate_frame_name(file, &condition, "if", &mut identifiers);
        instrument_decision(
            &mut insertions,
            runtime_path,
            file,
            &condition,
            "if",
            &frame_name,
        );
    }
    for expression in root.descendants().filter_map(ast::WhileExpr::cast) {
        let condition = expression
            .condition()
            .expect("a tree parsed without errors has every part its syntax requires");
        if has_let(&condition) {
            if !cannot_carry_probe(condition.syntax()) {
                instrument_let_chain(
                    &mut insertions,
                    runtime_path,
                    file,
                    &condition,
                    ChainHost::While(&expression),
                    &mut identifiers,
                )?;
            }
            continue;
        }
        let frame_name = allocate_frame_name(file, &condition, "while", &mut identifiers);
        instrument_decision(
            &mut insertions,
            runtime_path,
            file,
            &condition,
            "while",
            &frame_name,
        );
    }
    for guard in root.descendants().filter_map(ast::MatchGuard::cast) {
        // The grammar gives a guard its condition, and a file that does not
        // parse is refused before this.
        let condition = guard
            .condition()
            .expect("a parsed match guard has a condition");
        let frame_name = allocate_frame_name(file, &condition, "match-guard", &mut identifiers);
        instrument_decision(
            &mut insertions,
            runtime_path,
            file,
            &condition,
            "match-guard",
            &frame_name,
        );
    }
    // `assert!(cond, ...)`: the condition decides whether the program goes
    // on, and the macro takes an instrumented expression like any other.
    // Without a message of its own, `assert!` stringifies the condition into
    // the panic message -- which `#[should_panic(expected = "...")]` tests
    // read -- so the original text is supplied as the message, exactly as
    // the macro would have built it.
    for arguments in &assertions {
        let Some(condition) = assertion_condition(root, *arguments) else {
            continue;
        };
        let frame_name = allocate_frame_name(file, &condition, "assert", &mut identifiers);
        if !instrument_decision(
            &mut insertions,
            runtime_path,
            file,
            &condition,
            "assert",
            &frame_name,
        ) {
            continue;
        }
        if assertion_argument_count(root, *arguments) == 1 {
            let range = condition.syntax().text_range();
            let original = &source[usize::from(range.start())..usize::from(range.end())];
            push_direct(
                &mut insertions,
                usize::from(range.end()),
                format!(", \"assertion failed: {{}}\", stringify!({original})"),
            );
        }
    }
    // `matches!(e, pat)` is a boolean decision wherever it stands; as a
    // condition of an `if` it is already one of that decision's atoms.
    for expression in standalone_matches(root, &matches, &assertions) {
        let frame_name = allocate_frame_name(file, &expression, "matches", &mut identifiers);
        instrument_decision(
            &mut insertions,
            runtime_path,
            file,
            &expression,
            "matches",
            &frame_name,
        );
    }
    for expression in value_decisions(root, &assertions) {
        let frame_name = allocate_frame_name(file, &expression, "value", &mut identifiers);
        instrument_decision(
            &mut insertions,
            runtime_path,
            file,
            &expression,
            "value",
            &frame_name,
        );
    }

    // Nothing survives in code the compiler leaves out; a probe there would
    // name an obligation the manifest no longer has, if the item were built.
    // A probe at either edge belongs to its neighbour: the next statement's
    // probe goes right after a stripped statement's `;` (semver's
    // `#[cfg(target_endian = "big")] let ..;` took the probe of the line
    // after it). A wrapper goes or stays whole, by the span it encloses.
    if !compiled_out.is_empty() {
        let within = |start: usize, end: usize| {
            compiled_out
                .iter()
                .any(|range| usize::from(range.start()) <= start && end <= usize::from(range.end()))
        };
        let strictly_inside = |offset: usize| {
            compiled_out.iter().any(|range| {
                usize::from(range.start()) < offset && offset < usize::from(range.end())
            })
        };
        let mut kept = Vec::with_capacity(insertions.len());
        let mut pending = insertions.into_iter();
        while let Some(insertion) = pending.next() {
            if insertion.kind == InsertionKind::Direct {
                if !strictly_inside(insertion.offset) {
                    kept.push(insertion);
                }
                continue;
            }
            // `push_wrapper` pushes a wrapper's two halves together, and
            // nothing comes between them before here.
            let end = pending.next().expect("a wrapper's end follows its start");
            if !within(insertion.offset, end.offset) {
                kept.push(insertion);
                kept.push(end);
            }
        }
        insertions = kept;
    }
    // Only the assertion markers survive in test code, and only the sites
    // they name stay in the manifest.
    if !tests.is_empty() {
        let inside = |offset: usize| {
            tests.iter().any(|test| {
                usize::from(test.start()) <= offset && offset <= usize::from(test.end())
            })
        };
        let markers = manifest
            .assertion_sites
            .iter()
            .map(|site| {
                (
                    format!(
                        "{runtime_path}::assertion({});",
                        assertion_ordinal(&site.id)
                    ),
                    site.id.clone(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut marked = BTreeSet::new();
        insertions.retain(|insertion| {
            if !inside(insertion.offset) {
                return true;
            }
            match markers.get(&insertion.text) {
                Some(id) => {
                    marked.insert(id.clone());
                    true
                }
                None => false,
            }
        });
        manifest
            .assertion_sites
            .retain(|site| marked.contains(&site.id));
    }

    if skipped_attributed_statement && !test_file {
        add_manifest_limitation(
            &mut manifest,
            file,
            "rust-attributed-statement-probes-not-injected",
            "A `let` without an initializer that carries outer attributes has no expression to hold a probe",
        );
    }
    manifest.limitations.sort_by(|left, right| {
        left.get("id")
            .and_then(|value| value.as_str())
            .cmp(&right.get("id").and_then(|value| value.as_str()))
    });

    // Test faults stand in for insertions that overlap and for a transform
    // that no longer parses, which a correct instrumenter never makes.
    let code = crate::fault::or(
        "rust-insertions",
        || Err(RustInstrumenterError::InvalidRange),
        || apply_insertions(source, insertions),
    )?;
    let code = if crate::fault::injected("rust-unparsable-transform") {
        format!("{code} {{")
    } else {
        code
    };
    let code = still_parses(file, code, edition, || {
        std::env::var_os(FAILED_TRANSFORM_DUMP_ENV)
    })?;
    Ok(RustInstrumentedSource {
        code,
        manifest,
        probes: RustProbeLayout::default(),
    })
}

/// The transformed text, if it parses as the original did. When it does not,
/// the text is what a diagnosis needs -- the parse errors alone do not say
/// where -- and it goes to the directory `dump` names, only when one is
/// named, since it is the size of the source.
fn still_parses(
    file: &str,
    code: String,
    edition: Edition,
    dump: impl FnOnce() -> Option<std::ffi::OsString>,
) -> Result<String, RustInstrumenterError> {
    let errors = SourceFile::parse(&code, edition)
        .errors()
        .into_iter()
        .map(|error| error.to_string())
        .collect::<Vec<_>>();
    if errors.is_empty() {
        return Ok(code);
    }
    if let Some(directory) = dump() {
        let name = file.replace(['/', '\\'], "__");
        let _ = crate::fault_fs::create_dir_all(&directory);
        let _ = crate::fault_fs::write(std::path::Path::new(&directory).join(name), &code);
    }
    Err(RustInstrumenterError::Parse(errors))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    /// A block with nowhere to put its probe, at each place a block is
    /// entered, refuses the file rather than probing it wrongly.
    #[test]
    fn a_block_with_no_entry_refuses_its_file() {
        let source = concat!(
            "pub fn f(a: bool, b: Option<u8>, c: &[u8]) -> u8 {\n",
            "    if a && b.is_some() { 1 } else if let Some(x) = b && x > 2 { x } else { 0 };\n",
            "    let g = |n: u8| { n + 1 };\n",
            "    while a { break; }\n",
            "    for _ in c { continue; }\n",
            "    loop { break; }\n",
            "    match b { Some(1) => { 1 } Some(_) if a => 2, _ => { 3 } };\n",
            "    let Some(y) = b else { return 0 };\n",
            "    while let Some(z) = b && z > 1 { break; }\n",
            "    { let _ = 1; }\n",
            "    #[allow(unused)] { let _z = 1; }\n",
            "    #[allow(unused)] let _w = { 1 };\n",
            "    let h = || -> u8 { 2 };\n",
            "    g(y) + h()\n",
            "}\n",
        );
        let reached = crate::fault::fail_each_call(
            "rust-block-entry",
            || (),
            |()| instrument_rust_file("src/lib.rs", source, "crate::rt", false, None).map(drop),
            |()| {},
        );
        assert!(reached > 9, "{reached}");
        for fault in ["rust-insertions", "rust-unparsable-transform"] {
            let refused = crate::fault::during(fault, || {
                instrument_rust_file("src/lib.rs", source, "crate::rt", false, None)
            });
            assert!(refused.is_err(), "{fault}");
        }
    }

    #[test]
    fn each_instrumenter_error_says_what_failed() {
        use super::RustInstrumenterError as E;
        let messages = [
            E::SourceTooLarge,
            E::Parse(vec!["one".into(), "two".into()]),
            E::InvalidRange,
            E::InvalidRuntimePath,
            E::Internal("boom".into()),
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                "Rust source exceeds the parser range",
                "Rust parse failed: one; two",
                "Rust parser returned an invalid range",
                "invalid generated Rust runtime path",
                "an internal error: boom",
            ]
        );
    }

    use super::*;

    /// The names instrumentation generated in `code`, other than the
    /// runtime's.
    fn generated_names(code: &str) -> BTreeSet<String> {
        SourceFile::parse(code, Edition::Edition2024)
            .tree()
            .syntax()
            .descendants_with_tokens()
            .filter_map(|element| element.into_token())
            .filter(|token| token.kind() == SyntaxKind::IDENT)
            .map(|token| token.text().to_owned())
            .filter(|name| {
                name.to_ascii_lowercase().starts_with("__supercov_")
                    && !name.starts_with("__supercov_runtime")
            })
            .collect()
    }

    #[test]
    fn a_generated_name_the_source_already_uses_is_numbered_past_it() {
        let source = concat!(
            "pub fn f(o: Option<i32>, mut items: std::vec::IntoIter<i32>, n: i32) -> i32 {\n",
            "    let mut total = 0;\n",
            "    if let Some(x) = o && x > 0 { total += x; }\n",
            "    while let Some(y) = items.next() { total += y; }\n",
            "    let mut k = n;\n",
            "    while k > 0 { k -= 1; }\n",
            "    match n { 0 => total, _ => total + 1 }\n",
            "}\n",
        );
        let runtime = "crate::__supercov_runtime_v1";
        let first = instrument_rust_file("src/lib.rs", source, runtime, false, None).unwrap();
        let names = generated_names(&first.code);
        for kind in ["decision", "ARMS", "CHAIN", "loop", "broke"] {
            assert!(
                names.iter().any(|name| name
                    .to_ascii_lowercase()
                    .contains(&kind.to_ascii_lowercase())),
                "{kind}: {names:?}"
            );
        }
        // Each of them already taken, after the code that asks for it, so
        // its place and so its stable name are unchanged.
        let taken = names
            .iter()
            .map(|name| format!("pub fn {name}() {{}}\n"))
            .collect::<String>();
        let second = instrument_rust_file(
            "src/lib.rs",
            &format!("{source}{taken}"),
            runtime,
            false,
            None,
        )
        .unwrap();
        assert!(
            SourceFile::parse(&second.code, Edition::Edition2024)
                .errors()
                .is_empty(),
            "{}",
            second.code
        );
        let renamed = generated_names(&second.code);
        for name in &names {
            assert!(
                renamed.contains(&format!("{name}_1")),
                "{name}: {renamed:?}"
            );
        }
    }

    /// A limitation's kind: its ID without the file that scopes it.
    fn limitation_kind_of(limitation: &serde_json::Value) -> Option<&str> {
        limitation.get("id")?.as_str()?.split('#').next()
    }

    const NOOP_RUNTIME: &str = r#"
#[doc(hidden)]
mod __supercov_runtime_v1 {
    pub struct DecisionFrame;
    impl DecisionFrame {
        pub fn new(_: u32, _: usize) -> Self { Self }
    }
    pub fn hit(_: u32) {}
    pub fn arms(_: &[u32], _: usize) {}
    pub fn logical(left: bool, _: bool, _: u32, _: u32) -> bool { left }
    pub fn for_loop<I: IntoIterator>(iterable: I, _: u32, _: u32) -> I::IntoIter {
        iterable.into_iter()
    }
    pub fn entered(_: &mut bool, _: u32) {}
    pub fn zero_iterations(_: bool, _: u32) {}
    pub trait TryProbe: Sized {
        fn probe(self, _: u32, _: u32) -> Self { self }
    }
    impl<T> TryProbe for T {}
    pub fn condition<V: std::ops::Not<Output = bool>>(value: V, _: &mut DecisionFrame, _: usize) -> bool { !!value }
    pub fn decision(value: bool, _: &mut DecisionFrame) -> bool { value }
    pub fn reached(_: &mut DecisionFrame, _: usize) -> bool { true }
    pub fn decision_chain(_: &mut DecisionFrame, _: bool, _: &[(usize, u32, u32)]) {}
    pub fn assertion(_: u32) {}
}
"#;

    fn compile_and_run(source: &str, name: &str) -> std::process::Output {
        compile_and_run_edition(source, name, "2024")
    }

    fn compile_and_run_edition(source: &str, name: &str, edition: &str) -> std::process::Output {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "supercov-rust-transform-{}-{nonce}-{name}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let input = directory.join("main.rs");
        let binary = directory.join("program");
        fs::write(&input, source).unwrap();
        let compile = Command::new("rustc")
            .arg(format!("--edition={edition}"))
            .arg(&input)
            .arg("-o")
            .arg(&binary)
            .output()
            .unwrap();
        assert!(
            compile.status.success(),
            "rustc failed:\n{}\nsource:\n{source}",
            String::from_utf8_lossy(&compile.stderr)
        );
        let output = Command::new(&binary).output().unwrap();
        fs::remove_dir_all(directory).unwrap();
        output
    }

    #[test]
    fn a_match_and_a_loop_take_no_line_of_their_own() {
        // The table of a `match`'s arms, the hit of an arm that is a block,
        // a loop's entry and the two ends of an `if` over `&&` each took a
        // line. arrayvec's doctest at line 302 was named `(line 304)`, and a
        // panic below a `match` was reported two lines down.
        let source = r#"fn kind(key: &str) -> u8 {
    match key.len() {
        0 => 0,
        n if n < 4 => {
            1
        }
        _ => 2,
    }
}

fn scan(key: &str, limit: usize) -> usize {
    let mut seen = 0;
    while seen < limit && seen < key.len() {
        if key.as_bytes()[seen] == b'-' || seen > 8 {
            break;
        } else {
            seen += 1;
        }
    }
    for _ in 0..limit {
        seen += 0;
    }
    seen
}
"#;
        let transformed =
            instrument_rust_source("src/scan.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert_eq!(
            transformed.code.lines().count(),
            source.lines().count(),
            "{}",
            transformed.code
        );
        // And each line still ends as its author ended it.
        for (written, rewritten) in source.lines().zip(transformed.code.lines()) {
            let end = written.trim().trim_end_matches(['{', ';', ',']).trim();
            let last = end.rsplit(' ').next().unwrap_or_default();
            assert!(rewritten.contains(last), "{written:?} became {rewritten:?}");
        }
    }

    #[test]
    fn a_panic_is_reported_where_its_author_wrote_it() {
        // Rust reports a panic by line and column. The probe at a function's
        // start took a line of its own, so every line after the first
        // function was one further down for each function above it, and the
        // probe before a statement moved it along its line: `src/lib.rs:8:9`
        // read `9:84`. A program's panics are where they were.
        let source = r#"fn check(key: &str) -> usize {
    if key.len() < 4 {
        panic!("key too short");
    }
    key.len()
}

fn later(values: &[usize]) -> usize {
    let first = values[0];
    let total: usize = values.iter().sum();
    assert!(total > first, "nothing but the first");
    total
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    if mode == "later" {
        later(&[3]);
    }
    let closure = |key: &str| {
        check(key)
    };
    closure("ab");
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert_eq!(
            transformed.code.lines().count(),
            source.lines().count(),
            "{}",
            transformed.code
        );
        // Every line of the program still holds what it held, where it was.
        for (number, (written, rewritten)) in
            source.lines().zip(transformed.code.lines()).enumerate()
        {
            let code = written.trim_start();
            // Where the statement begins: its first word.
            let word = code.split(['(', ' ']).next().unwrap_or(code);
            if matches!(word, "panic!" | "assert!" | "let") {
                assert_eq!(
                    rewritten.find(word),
                    written.find(word),
                    "line {}: {rewritten}",
                    number + 1
                );
            }
        }
        let location = |output: &std::process::Output| {
            String::from_utf8_lossy(&output.stderr)
                .lines()
                .find(|line| line.contains("panicked at"))
                .map(|line| line[line.find("main.rs").unwrap()..].to_owned())
                .expect("a panic")
        };
        let original = compile_and_run(source, "panic-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "panic-instrumented",
        );
        assert_eq!(location(&original), "main.rs:3:9:");
        assert_eq!(location(&instrumented), location(&original));
    }

    #[test]
    fn a_target_cfg_decides_only_what_the_target_settles() {
        let cfg = TargetCfg::new(
            &[
                "unix",
                "target_os=\"linux\"",
                "target_family=\"unix\"",
                "target_pointer_width=\"64\"",
                "debug_assertions",
            ]
            .map(|value| value.parse::<cargo_platform::Cfg>().unwrap()),
        );
        let decide = |expression: &str| cfg.evaluate(&expression.parse().unwrap());
        assert_eq!(decide("unix"), Some(true));
        assert_eq!(decide("windows"), Some(false));
        assert_eq!(decide("target_os = \"macos\""), Some(false));
        assert_eq!(decide("not(target_os = \"linux\")"), Some(false));
        assert_eq!(
            decide("all(unix, target_pointer_width = \"64\")"),
            Some(true)
        );
        assert_eq!(decide("any(windows, target_os = \"wasi\")"), Some(false));
        // A feature, `test` or a custom cfg is not the target's to settle.
        assert_eq!(decide("feature = \"extra\""), None);
        assert_eq!(decide("not(feature = \"extra\")"), None);
        assert_eq!(decide("debug_assertions"), None);
        assert_eq!(decide("all(windows, feature = \"extra\")"), Some(false));
        assert_eq!(decide("any(unix, feature = \"extra\")"), Some(true));
        assert_eq!(decide("any(windows, feature = \"extra\")"), None);
        assert_eq!(decide("true"), Some(true));
        assert_eq!(decide("false"), Some(false));
        // A target that names nothing settles only what is no name.
        assert_eq!(
            TargetCfg::new(&[]).evaluate(&"unix".parse().unwrap()),
            Some(false)
        );

        let source = "pub fn a() -> u8 { b() }\n#[cfg(windows)]\nfn b() -> u8 { 1 }\n#[cfg(unix)]\nfn b() -> u8 { 2 }\n#[cfg(feature = \"x\")]\nfn c() -> u8 { 3 }\n#[cfg(unix)]\n#[cfg(windows)]\nfn d() -> u8 {\n    4\n}\npub fn e(a: bool, b: bool) -> bool {\n    a && (b || a)\n}\n";
        let instrumented = instrument_rust_file(
            "src/lib.rs",
            source,
            "crate::__supercov_runtime_v1",
            false,
            Some(&cfg),
        )
        .unwrap();
        let lines = instrumented
            .manifest
            .points
            .iter()
            .map(|point| point.line)
            .collect::<BTreeSet<_>>();
        assert!(
            !lines.contains(&3),
            "the windows item is left out: {lines:?}"
        );
        assert!(lines.contains(&5) && lines.contains(&7), "{lines:?}");
        // Two predicates on one item hold together: unix and windows, never.
        assert!(!lines.contains(&11), "{lines:?}");
        // A decision whose conditions nest keeps every probe beside code
        // compiled out.
        assert!(lines.contains(&14), "{lines:?}");
        assert_eq!(
            instrumented
                .manifest
                .decisions
                .iter()
                .filter(|d| d.line == 14)
                .count(),
            1
        );
        let windows = &instrumented.code[instrumented.code.find("#[cfg(windows)]").unwrap()
            ..instrumented.code.find("#[cfg(unix)]").unwrap()];
        assert!(!windows.contains("__supercov_runtime_v1"), "{windows}");
    }

    #[test]
    fn code_behind_a_cfg_only_the_build_settles_is_gated_by_its_predicate() {
        let cfg = TargetCfg::new(
            &["unix", "target_os=\"linux\""]
                .map(|value| value.parse::<cargo_platform::Cfg>().unwrap()),
        );
        let source = "\
pub fn a() -> u8 {
    #[cfg(windows)]
    let v = 1;
    #[cfg(unix)]
    let v = 2;
    #[cfg(debug_assertions)]
    let w = v + 1;
    #[cfg(not(debug_assertions))]
    let w = v;
    w
}
#[cfg(all(unix, feature = \"x y\"))]
fn c() -> u8 {
    3
}
#[cfg(feature = \"z\")]
struct NoCode;
#[cfg(test)]
mod tests {
    #[test]
    fn t() {}
}
";
        let instrumented = instrument_rust_file(
            "src/lib.rs",
            source,
            "crate::__supercov_runtime_v1",
            false,
            Some(&cfg),
        )
        .unwrap();
        let lines = instrumented
            .manifest
            .points
            .iter()
            .map(|point| (point.line, point.id.clone()))
            .collect::<Vec<_>>();
        // The target settles statements as it settles items.
        assert!(lines.iter().all(|(line, _)| *line != 3), "{lines:?}");
        let gates = &instrumented.probes.cfgs;
        let described = gates
            .iter()
            .map(|gate| (gate.predicate.as_str(), gate.line, gate.end_line))
            .collect::<Vec<_>>();
        // Only code with obligations is gated; test code is not measured.
        assert_eq!(
            described,
            [
                ("debug_assertions", 6, 7),
                ("not(debug_assertions)", 8, 9),
                ("all(unix, feature = \"x y\")", 12, 15),
            ],
            "{gates:?}"
        );
        let in_c = lines
            .iter()
            .filter(|(line, _)| (13..=15).contains(line))
            .map(|(_, id)| id.clone())
            .collect::<BTreeSet<_>>();
        assert!(!in_c.is_empty());
        assert_eq!(
            gates[2]
                .obligations
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>(),
            in_c
        );
        // Without a target, the target's own keys are the build's to settle.
        let unsettled = instrument_rust_file(
            "src/lib.rs",
            source,
            "crate::__supercov_runtime_v1",
            false,
            None,
        )
        .unwrap();
        assert!(
            unsettled
                .probes
                .cfgs
                .iter()
                .any(|gate| gate.predicate == "windows" && gate.line == 2),
            "{:?}",
            unsettled.probes.cfgs
        );
    }

    #[test]
    fn test_code_is_known_by_where_it_sits() {
        for file in [
            "tests/it.rs",
            "tests/common/mod.rs",
            "crates/a/tests/it.rs",
            "examples/demo.rs",
            "benches/speed.rs",
            "src/tests.rs",
            "src/parser_tests.rs",
            "src/parser-tests.rs",
        ] {
            assert!(test_code_path(file), "{file}");
        }
        for file in [
            "src/lib.rs",
            "src/attests.rs",
            "src/_tests.rs",
            "crates/foo-tests/src/lib.rs",
            "src/testing.rs",
        ] {
            assert!(!test_code_path(file), "{file}");
        }
    }

    #[test]
    fn test_items_keep_only_their_assertion_markers() {
        let source = r#"pub fn add(a: i32, b: i32) -> i32 {
    if a > 0 && b > 0 { a + b } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adds() {
        let total = add(1, 2);
        assert_eq!(total, 3);
    }
}

#[cfg(all(test, feature = "slow"))]
fn slow_helper() -> bool { true || false }

#[tokio::test]
async fn async_adds() {
    assert!(add(2, 2) == 4);
}
"#;
        let transformed =
            instrument_rust_source("src/lib.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let manifest = &transformed.manifest;
        assert!(
            manifest.points.iter().all(|point| point.line <= 3),
            "{:?}",
            manifest.points
        );
        assert!(manifest.decisions.iter().all(|decision| decision.line == 2));
        assert!(manifest.branches.iter().all(|branch| branch.line == 2));
        assert_eq!(manifest.decisions.len(), 1);
        let sites = manifest
            .assertion_sites
            .iter()
            .map(|site| site.source.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            sites,
            BTreeSet::from(["assert!(add(2, 2) == 4);", "assert_eq!(total, 3);"])
        );
        let tests = &transformed.code[transformed.code.find("#[cfg(test)]").unwrap()..];
        assert_eq!(tests.matches("::assertion(").count(), 2, "{tests}");
        assert!(!tests.contains("::hit("), "{tests}");
        assert!(!tests.contains("::logical("), "{tests}");

        // A whole test file the same way.
        let file = instrument_rust_source(
            "tests/it.rs",
            "#[test]\nfn it() {\n    let x = 1;\n    assert_eq!(x, 1);\n}\n",
            "crate::__supercov_runtime_v1",
        )
        .unwrap();
        assert!(file.manifest.points.is_empty());
        assert_eq!(file.manifest.assertion_sites.len(), 1);
        assert!(!file.code.contains("::hit("));
    }

    #[test]
    fn discovers_rust_obligations_with_exact_ranges_and_stable_ids() {
        let source = r#"fn classify<T>(values: &[T], first: bool, second: bool, third: bool) -> Option<&T> {
    let picked = if first && (second || third) {
        values.first()?
    } else {
        None
    };
    for value in values {
        if first || second {
            return Some(value);
        }
    }
    match picked {
        Some(value) if second && third => Some(value),
        _ => None,
    }
}

fn closure(value: i32) -> bool {
    (|candidate| candidate > 0)(value)
}
"#;
        let first = build_rust_manifest("src/lib.rs", source).unwrap();
        let second = build_rust_manifest("src/lib.rs", source).unwrap();
        assert_eq!(first, second);
        assert!(first.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("classify")
        }));
        assert!(first.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("<closure>")
        }));
        let first_if = first
            .decisions
            .iter()
            .find(|decision| decision.line == 2)
            .unwrap();
        assert_eq!(first_if.conditions, ["first", "second", "third"]);
        assert_eq!(first_if.column, 20);
        assert!(
            first
                .branches
                .iter()
                .any(|branch| branch.kind == "for-loop")
        );
        let mut arms = first
            .branches
            .iter()
            .filter(|branch| branch.kind == "match-arm")
            .collect::<Vec<_>>();
        arms.sort_by_key(|branch| branch.line);
        assert_eq!(arms.len(), 2);
        assert_eq!(
            arms[0]
                .alternatives
                .iter()
                .map(|alternative| alternative.label.as_str())
                .collect::<Vec<_>>(),
            ["not selected", "selected"]
        );
        // The last arm of an exhaustive match is reached or not; it is never
        // considered and passed over.
        assert_eq!(
            arms[1]
                .alternatives
                .iter()
                .map(|alternative| alternative.label.as_str())
                .collect::<Vec<_>>(),
            ["selected"]
        );
        assert!(
            first
                .branches
                .iter()
                .any(|branch| branch.kind == "try-operator")
        );
        assert!(first.decisions.iter().all(|decision| {
            decision.id.starts_with("rs:decision:") && decision.conditions.len() >= 2
        }));
        assert!(first.limitations.is_empty());
    }

    #[test]
    fn declares_macro_and_const_boundaries_instead_of_hiding_them() {
        let source = r#"const fn doubled(value: usize) -> usize { value * 2 }

macro_rules! noop {
    () => {};
}

fn checked(value: bool) -> bool {
    assert!(value);
    noop!();
    const { doubled(2) == 4 }
}
"#;
        let manifest = build_rust_manifest("src/lib.rs", source).unwrap();
        // The assertion is a decision of its own; the crate's own macro keeps
        // the macro limitation.
        assert!(manifest.decisions.iter().any(|decision| {
            decision.line == 8 && decision.source == "value" && decision.conditions == ["value"]
        }));
        let ids = manifest
            .limitations
            .iter()
            .filter_map(limitation_kind_of)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            ids,
            BTreeSet::from([
                "rust-const-context-not-instrumented",
                "rust-macro-expansion-not-instrumented"
            ])
        );
        assert!(!manifest.points.iter().any(|point| {
            point.kind == PointKind::Function && point.label.as_deref() == Some("doubled")
        }));
    }

    /// The macro limitation names four macros and counts the rest, in the
    /// singular where there is one, and quotes a long first line cut short.
    #[test]
    fn the_macro_limitation_names_four_and_counts_the_rest() {
        let reason = |count: usize, long: bool| {
            let mut source = String::new();
            for at in 0..count {
                source.push_str(&format!("macro_rules! m{at} {{ ($($t:tt)*) => {{}}; }}\n"));
            }
            source.push_str("pub fn f() {\n");
            for at in 0..count {
                if long && at == 0 {
                    source.push_str(&format!("    m0!({});\n", "x ".repeat(80)));
                } else {
                    source.push_str(&format!("    m{at}!();\n"));
                }
            }
            source.push_str("}\n");
            let manifest = build_rust_manifest("src/lib.rs", &source).unwrap();
            manifest
                .limitations
                .iter()
                .find(|limitation| {
                    limitation_kind_of(limitation) == Some("rust-macro-expansion-not-instrumented")
                })
                .map(|limitation| limitation.to_string())
                .unwrap()
        };
        assert!(
            reason(5, false).contains("and 1 other macro expand"),
            "{}",
            reason(5, false)
        );
        assert!(
            reason(6, false).contains("and 2 other macros expand"),
            "{}",
            reason(6, false)
        );
        assert!(
            reason(4, false).contains("`m3!` expand"),
            "{}",
            reason(4, false)
        );
        let long = reason(1, true);
        assert!(long.contains("x x...\""), "{long}");
    }

    #[test]
    fn a_vec_macro_that_starts_a_larger_expression_keeps_its_probe_outside() {
        // smallvec's `vec![elem; n].into()`: blanking `vec!` to read the
        // array moved where the statement starts, and its probe landed
        // between `vec!` and the brackets, which does not parse.
        let source = "pub fn grown(n: usize) -> Vec<u8> {\n    if n > 0 {\n        vec![0; n].into()\n    } else {\n        vec![1; 2] == vec![1, 1] && n == 0;\n        Vec::new()\n    }\n}\n";
        let transformed =
            instrument_rust_source("src/lib.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert!(
            !transformed.code.contains("vec!crate"),
            "{}",
            transformed.code
        );
        assert!(
            transformed.code.contains("vec![0; n].into()"),
            "{}",
            transformed.code
        );
    }

    #[test]
    fn transforms_points_and_nested_decisions_without_changing_behavior() {
        let source = r#"use std::sync::atomic::{AtomicUsize, Ordering};

static CALLS: AtomicUsize = AtomicUsize::new(0);

fn observed(name: &str, value: bool) -> bool {
    let order = CALLS.fetch_add(1, Ordering::SeqCst);
    println!("{order}:{name}:{value}");
    value
}

fn classify(first: bool, second: bool, third: bool) -> i32 {
    if observed("a", first) && (observed("b", second) || observed("c", third)) {
        7
    } else {
        3
    }
}

fn main() {
    let closure = |value: i32| value + 1;
    println!("result={}", closure(classify(true, false, true)));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert!(transformed.code.contains("::condition("));
        assert!(transformed.code.contains("::decision("));
        assert!(transformed.code.contains("::hit("));
        let original = compile_and_run(source, "original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn let_chains_take_derived_condition_probes_and_const_contexts_stay_declared() {
        let source = r#"const fn enabled(value: bool) -> bool {
    if value { true } else { false }
}

fn classify(value: Option<bool>, fallback: bool) -> bool {
    if let Some(inner) = value && inner && fallback { true } else { false }
}
"#;
        let transformed =
            instrument_rust_source("src/lib.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(limitation_kind_of)
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("rust-const-context-not-instrumented"));
        assert!(!ids.contains("rust-let-chain-probes-not-injected"));
        // The chain: a marker at the front, ordinary conditions wrapped, the
        // outcome recorded in both branches; the `let` itself untouched.
        assert!(
            transformed
                .code
                .contains("::reached(&mut __supercov_decision_")
        );
        assert!(
            transformed
                .code
                .contains("::condition((inner), &mut __supercov_decision_")
        );
        assert!(
            transformed
                .code
                .contains("::decision_chain(&mut __supercov_decision_")
        );
        assert!(transformed.code.contains("&& let Some(inner) = value &&"));
        assert!(!transformed.code.contains("condition((let"));
    }

    #[test]
    fn std_macro_arguments_take_probes_and_assertions_are_decisions() {
        let source = r#"use std::fmt::Write as _;

fn classify(values: &[i32], strict: bool) -> String {
    let mut out = String::new();
    assert!(values.len() < 10 && (strict || !values.is_empty()), "bad input {:?}", values);
    debug_assert!(values.iter().all(|v| *v > -100));
    let doubled = vec![values.iter().map(|v| v * 2).sum::<i32>(), if strict { 1 } else { 2 }];
    let repeated = vec![if strict { 1 } else { 0 }; values.len()];
    let small = matches!(values.first(), Some(v) if *v < 3);
    if matches!(values.len(), 1 | 2) && small {
        println!("small");
    }
    println!("{}", repeated.len() + small as usize);
    write!(out, "{}", doubled.iter().map(|d| if *d > 4 { "big" } else { "small" }).collect::<Vec<_>>().join(",")).unwrap();
    println!("{} {}", format!("{:?}", doubled), if values.first().copied().unwrap_or(0) > 0 && strict { "positive" } else { "other" });
    assert_eq!(doubled.len(), if strict { 2 } else { 2 }, "length for strict={strict}");
    out
}

fn main() {
    println!("{}", classify(&[1, 2], true));
    println!("{}", classify(&[3], false));
    println!("{}", classify(&[], true));
    let total: i32 = dbg!(vec![1, 2, 3]).into_iter().sum();
    println!("{total}");
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // `debug_assert!(cond)` had no message: the condition's own text is
        // supplied so the panic message is the one the macro would build.
        assert!(transformed.code.contains(
            r#", "assertion failed: {}", stringify!(values.iter().all(|v| *v > -100)))"#
        ));
        // `assert!(cond, "msg", args)` keeps its message untouched.
        assert!(
            transformed
                .code
                .contains(r#"), "bad input {:?}", values);"#)
        );
        // The assertion's condition is a two-condition decision, the `if`
        // inside `vec!` and `println!` are decisions, the logical operators
        // inside the macros are branches, and no macro is left declared.
        let assertion = transformed
            .manifest
            .decisions
            .iter()
            .find(|decision| decision.line == 5)
            .expect("assert! decision");
        assert_eq!(
            assertion.conditions,
            ["values.len() < 10", "strict", "!values.is_empty()"]
        );
        assert!(
            transformed
                .manifest
                .decisions
                .iter()
                .any(|decision| decision.line == 7)
        );
        assert!(
            transformed
                .manifest
                .decisions
                .iter()
                .any(|decision| decision.line == 9)
        );
        assert!(
            transformed
                .code
                .contains("assert!(({ let mut __supercov_decision_")
        );
        assert!(
            transformed
                .code
                .contains(", if ({ let mut __supercov_decision_")
        );
        // `vec![x; n]`: the element takes probes as an array element would.
        assert!(
            transformed
                .code
                .contains("vec![if ({ let mut __supercov_decision_")
        );
        // A standalone `matches!` is a decision; one that is already an `if`
        // condition's atom is not doubled.
        assert!(
            transformed
                .code
                .contains("let small = ({ let mut __supercov_decision_")
        );
        assert_eq!(
            transformed
                .manifest
                .decisions
                .iter()
                .filter(|decision| decision.line == 10)
                .count(),
            1
        );
        assert_eq!(
            transformed
                .manifest
                .decisions
                .iter()
                .filter(|decision| decision.line == 9)
                .count(),
            1
        );
        assert!(
            transformed
                .code
                .contains("vec![values.iter().map(|v| { crate::__supercov_runtime_v1::hit(")
        );
        assert!(!transformed.manifest.limitations.iter().any(|limitation| {
            limitation.get("id").and_then(|id| id.as_str())
                == Some("rust-macro-expansion-not-instrumented")
        }));
        let original = compile_and_run(source, "original-macros");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented-macros",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        // `dbg!` prints its own file:line:column, which the probes move;
        // compare what follows the location.
        let after_location = |stderr: &[u8]| {
            String::from_utf8_lossy(stderr)
                .lines()
                .map(|line| {
                    line.split_once("] ")
                        .map_or(line, |(_, rest)| rest)
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            after_location(&instrumented.stderr),
            after_location(&original.stderr)
        );
    }

    #[test]
    fn assertion_panic_messages_survive_instrumentation() {
        // smallvec's `#[should_panic(expected = "new_capacity >= len")]` reads
        // the message `assert!` builds from its condition's text.
        let source = r#"fn grow(len: usize, new_capacity: usize) {
    assert!(new_capacity >= len);
}

fn check(value: i32) {
    assert!(value > 0 && value < 10, "value {value} out of range");
}

// tokio: `assert!` only negates its operand, so a `&bool` is accepted.
fn all_seen(seen: &[bool]) {
    for was_seen in seen {
        assert!(was_seen);
        debug_assert!(was_seen, "seen");
    }
}

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    all_seen(&[true, true]);
    match std::panic::catch_unwind(|| all_seen(&[true, false])) {
        Ok(()) => println!("ok"),
        Err(payload) => println!("{}", payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_default()),
    }
    for (len, capacity) in [(3, 5), (8, 5)] {
        match std::panic::catch_unwind(|| grow(len, capacity)) {
            Ok(()) => println!("ok"),
            Err(payload) => println!("{}", payload.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_default()),
        }
    }
    match std::panic::catch_unwind(|| check(12)) {
        Ok(()) => println!("ok"),
        Err(payload) => println!("{}", payload.downcast_ref::<String>().cloned().unwrap_or_default()),
    }
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let original = compile_and_run(source, "original-assert-message");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented-assert-message",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert!(
            String::from_utf8_lossy(&instrumented.stdout)
                .contains("assertion failed: new_capacity >= len")
        );
        assert!(
            String::from_utf8_lossy(&instrumented.stdout).contains("assertion failed: was_seen")
        );
    }

    #[test]
    fn files_that_predate_a_reserved_word_still_instrument() {
        // itertools' tests call `rng.gen()`; `gen` is a keyword in 2024 only.
        let source = r#"struct Rng(u64);
impl Rng {
    fn gen(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.0 >> 33
    }
}

fn main() {
    let mut rng = Rng(7);
    let mut odd = 0;
    for _ in 0..10 {
        if rng.gen() % 2 == 1 {
            odd += 1;
        }
    }
    println!("{odd}");
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert!(
            transformed
                .manifest
                .decisions
                .iter()
                .any(|decision| decision.line == 13)
        );
        let original = compile_and_run_edition(source, "original-gen", "2021");
        let instrumented = compile_and_run_edition(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented-gen",
            "2021",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
    }

    #[test]
    fn lone_let_conditions_compile_before_edition_2024_and_record_breaks() {
        // bytes and memchr are edition 2018/2021: a plain `if let` there must
        // not become a let chain. `while let` needs its `break`s told apart
        // from the condition failing, including labeled ones from inner loops.
        let source = r#"fn first_even(values: &[i32]) -> Option<i32> {
    let mut it = values.iter();
    'scan: while let Some(value) = it.next() {
        if *value < 0 {
            break;
        }
        for _ in 0..1 {
            if *value == 99 {
                break 'scan;
            }
            if *value == 98 {
                break;
            }
        }
        if *value % 2 == 0 {
            return Some(*value);
        }
    }
    None
}

fn describe(value: Option<i32>) -> &'static str {
    if let Some(inner) = value {
        if inner > 0 { "positive" } else { "non-positive" }
    } else if let None = value {
        "none"
    } else {
        "unreachable"
    }
}

fn count(values: &[Option<i32>]) -> usize {
    let mut total = 0;
    for value in values {
        if let Some(_) = value {
            total += 1;
        }
    }
    total
}

fn main() {
    println!("{:?} {:?} {:?} {:?}", first_even(&[1, 3, 4]), first_even(&[1, -1, 4]), first_even(&[99, 4]), first_even(&[98, 3, 6]));
    println!("{} {} {}", describe(Some(2)), describe(Some(-2)), describe(None));
    println!("{}", count(&[Some(1), None, Some(3)]));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert!(!transformed.code.contains("&& let"));
        assert!(transformed.code.contains("__supercov_broke_"));
        assert_eq!(transformed.code.matches("= true; break").count(), 2);
        for edition in ["2021", "2024"] {
            let original =
                compile_and_run_edition(source, &format!("original-lone-let-{edition}"), edition);
            let instrumented = compile_and_run_edition(
                &format!("{}\n{NOOP_RUNTIME}", transformed.code),
                &format!("instrumented-lone-let-{edition}"),
                edition,
            );
            assert_eq!(instrumented.status, original.status);
            assert_eq!(instrumented.stdout, original.stdout);
            assert_eq!(instrumented.stderr, original.stderr);
        }
    }

    #[test]
    fn let_chains_keep_their_behavior() {
        let source = r#"fn describe(value: Option<i32>, flag: bool) -> &'static str {
    if let Some(inner) = value && inner > 0 && flag {
        "positive"
    } else if let Some(inner) = value && (inner < 0 || flag) {
        "negative-or-flagged"
    } else {
        "other"
    }
}

fn count_pairs(values: &[(Option<i32>, i32)]) -> i32 {
    let mut total = 0;
    let mut it = values.iter();
    while let Some((first, second)) = it.next() && let Some(inner) = first && *second > 0 {
        total += inner * second;
        if total > 100 {
            break;
        }
    }
    total
}

fn tail(value: Option<&str>) -> usize {
    let pick = |v: Option<&str>| if let Some(text) = v && !text.is_empty() { text.len() } else { 0 };
    if let Some(text) = value && text.starts_with('x') {
        println!("x-prefixed");
    }
    pick(value)
}

fn main() {
    for value in [Some(3), Some(-3), Some(0), None] {
        for flag in [true, false] {
            println!("{value:?} {flag} {}", describe(value, flag));
        }
    }
    println!("{}", count_pairs(&[(Some(2), 3), (Some(4), 5), (None, 1), (Some(9), 9)]));
    println!("{}", count_pairs(&[(Some(50), 3), (Some(4), 5)]));
    println!("{} {} {}", tail(Some("xyz")), tail(Some("")), tail(None));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert_eq!(
            transformed.code.matches("const __SUPERCOV_CHAIN_").count(),
            5
        );
        assert!(!transformed.manifest.limitations.iter().any(|limitation| {
            limitation.get("id").and_then(|id| id.as_str())
                == Some("rust-let-chain-probes-not-injected")
        }));
        let original = compile_and_run(source, "original-chains");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented-chains",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    /// A runtime path is `crate::` and identifiers; a test file is one
    /// under tests, examples or benches, or named for tests.
    #[test]
    fn runtime_paths_and_test_files_are_read_by_their_names() {
        for (path, valid) in [
            ("crate::supercov_runtime", true),
            ("crate::a::_b2", true),
            ("crate", false),
            ("crate::", false),
            ("crate::a::", false),
            ("crate::2a", false),
            ("crate::a-b", false),
            ("self::a", false),
        ] {
            assert_eq!(super::valid_runtime_path(path), valid, "{path}");
        }
        for (file, test) in [
            ("tests/it.rs", true),
            ("crates/a/benches/b.rs", true),
            ("examples\\demo.rs", true),
            ("src/tests.rs", true),
            ("src/parser_tests.rs", true),
            ("src/parser-tests.rs", true),
            ("src/my_parser-tests.rs", true),
            ("src/my-parser_tests.rs", true),
            ("src/_tests.rs", false),
            ("src/a.b_tests.rs", false),
            ("src/contests.rs", false),
            ("src/lib.rs", false),
        ] {
            assert_eq!(super::test_code_path(file), test, "{file}");
        }
    }

    /// Shapes a probe has to be placed around without breaking the file:
    /// each instruments, and what comes out still parses.
    #[test]
    fn every_shape_instruments_to_source_that_parses() {
        let shapes = [
            "fn a(x: Option<u8>) -> u8 {\n    let Some(v) = x else { return 0 };\n    if let Some(w) = x && w > 1 { w } else { v }\n}\n",
            "fn b(x: u8) -> u8 {\n    match x {\n        0 => 1,\n        n if n > 5 && n < 9 => 2,\n        _ => {\n            3\n        }\n    }\n}\n",
            "fn c(v: &[u8]) -> usize {\n    let mut n = 0;\n    'outer: loop {\n        for x in v {\n            while *x > 0 && n < 3 {\n                n += 1;\n                continue 'outer;\n            }\n        }\n        break n;\n    }\n}\n",
            "const K: u8 = if true { 1 } else { 2 };\nstatic S: [u8; 2] = [K, K];\nfn d() -> u8 { K }\n",
            "fn e(a: bool, b: bool) -> bool {\n    assert!(a || b, \"one of them\");\n    debug_assert!(a && !b);\n    assert_eq!(a, b);\n    (a)\n}\n",
            "fn f() -> Result<u8, String> {\n    let x: u8 = \"1\".parse().map_err(|e: std::num::ParseIntError| e.to_string())?;\n    Ok(x)\n}\n",
            "fn g(a: bool) {\n    let _c = || if a { 1 } else { 2 };\n    let _b = async { a };\n    unsafe { std::hint::unreachable_unchecked() }\n}\n",
            "trait T {\n    fn t(&self) -> bool { true }\n    const C: u8 = 1;\n}\nimpl T for () {}\n",
            "macro_rules! m { ($e:expr) => { $e }; }\nfn h(a: bool) -> bool { m!(a && a) }\n",
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn it() { assert!(1 + 1 == 2); }\n}\n",
            "fn i(a: u8) -> u8 {\n    let x = { a + 1 };\n    #[allow(unused)]\n    let y = a;\n    vec![x, y].len() as u8\n}\n",
            "fn j(c: bool) -> u8 {\n    'blk: {\n        if c {\n            break 'blk 1;\n        }\n        loop {\n            break;\n        }\n        while c {\n            break;\n        }\n        for _ in 0..1 {\n            break;\n        }\n        let _f = || loop {\n            break;\n        };\n        2\n    }\n}\n",
            "fn k(xs: &[u8]) {\n    for x in xs {\n        assert!(*x > 0)\n    }\n    if xs.is_empty() {\n        assert!(true)\n    } else {\n        assert_eq!(xs.len(), xs.len())\n    }\n}\n",
            "fn l(a: bool) -> bool {\n    #[allow(unused_must_use)]\n    assert!(a || !a);\n    #[cfg(any())]\n    let gone = 1;\n    #[allow(unused)]\n    let kept = a && !a;\n    kept\n}\n",
            "fn m(v: Option<u8>) -> u8 {\n    match v {\n        Some(x) if let Some(y) = Some(x) && y > 1 => y,\n        Some(x) => x,\n        None => 0,\n    }\n}\n",
            "const fn n(mut i: u8) -> u8 {\n    while i < 3 && i != 9 {\n        i += 1;\n    }\n    loop {\n        if i > 1 {\n            break;\n        }\n    }\n    match i {\n        0 => 1,\n        _ if i > 2 => 2,\n        _ => {\n            3\n        }\n    }\n}\n",
            "fn o(n: std::convert::Infallible) -> u8 {\n    match n {}\n}\n",
            "fn p(a: bool) -> u8 {\n    let f = || -> u8 {\n        if a { 1 } else { 2 }\n    };\n    let v = vec![f(), 2];\n    let w = [f(); 2];\n    println!(\"{:?} {:?}\", v, w);\n    #[rustfmt::skip]\n    println!(\"{}\", a);\n    f()\n}\n",
            // A loop's own breaks, past breaks of loops and closures inside it.
            "fn r(c: bool) -> u8 {\n    loop {\n        while c {\n            break;\n        }\n        for _ in 0..1 {\n            break;\n        }\n        let _g = || loop {\n            break;\n        };\n        'inner: loop {\n            break 'inner;\n        }\n        break 1;\n    }\n}\n",
            // A let chain whose first operand holds no `let`.
            "fn s(a: bool, b: Option<u8>) -> u8 {\n    if a && let Some(x) = b && x > 1 { x } else { 0 }\n}\n",
            "fn q(a: Option<u8>) -> Option<u8> {\n    let b = a?;\n    let c = if let Some(x) = a { x } else { b };\n    while let Some(y) = a {\n        if y > c { break; }\n    }\n    Some(c)\n}\n",
        ];
        for shape in shapes {
            let out = super::instrument_rust_source("src/lib.rs", shape, "crate::supercov_runtime")
                .unwrap_or_else(|error| panic!("{error:?}\n{shape}"));
            let parsed =
                ra_ap_syntax::SourceFile::parse(&out.code, ra_ap_syntax::Edition::Edition2024);
            assert!(
                parsed.errors().is_empty(),
                "{:?}\n{}",
                parsed.errors(),
                out.code
            );
        }
        assert!(matches!(
            super::instrument_rust_source("src/lib.rs", "fn a() {}\n", "self::rt"),
            Err(super::RustInstrumenterError::InvalidRuntimePath)
        ));
    }

    /// Test code is what `cfg(test)` gates, alone or first in an `all(..)`,
    /// and what a test or bench attribute marks, by its last path segment.
    #[test]
    fn test_items_are_known_by_their_attributes() {
        let source = "#[cfg(test)]\nfn a() {}\n#[cfg(all(test))]\nfn b() {}\n#[cfg(all(test, unix))]\nfn c() {}\n#[bench]\nfn d() {}\n#[tokio::test]\nasync fn e() {}\n#[cfg(not(test))]\nfn f() {}\n#[test_case]\nfn g() {}\n#[derive(Debug)]\nstruct H;\n#[cfg(all(unix, test))]\nfn i() {}\n";
        let tree =
            ra_ap_syntax::SourceFile::parse(source, ra_ap_syntax::Edition::Edition2024).tree();
        use ra_ap_syntax::{AstNode, ast::HasName};
        let marked = tree
            .syntax()
            .descendants()
            .filter_map(ra_ap_syntax::ast::Fn::cast)
            .map(|function| {
                (
                    function.name().unwrap().text().to_string(),
                    super::test_item(function.syntax()),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            marked,
            [
                ("a", true),
                ("b", true),
                ("c", true),
                ("d", true),
                ("e", true),
                ("f", false),
                ("g", false),
                ("i", false),
            ]
            .map(|(name, test)| (name.to_owned(), test))
        );
    }

    #[test]
    fn instrumented_const_and_static_initialisers_still_compile() {
        // Every one of these positions is const-evaluated, so none of them can
        // hold a call to the runtime -- `condition`, `decision` and `hit` are
        // not `const fn`. Found on bytes-1.12.1, whose test target has
        // `const ITERS: usize = if cfg!(miri) { 100 } else { 1_000 };` and
        // failed to build with E0015.
        let source = r#"const DIRECT: usize = if cfg!(unix) { 100 } else { 1_000 };
static WIDTH: usize = if cfg!(unix) { 2 } else { 4 };

enum Mode {
    Narrow = if cfg!(unix) { 1 } else { 2 },
}

struct Buffer([u8; if cfg!(unix) { 4 } else { 8 }]);

impl Buffer {
    const SPAN: usize = if cfg!(unix) { 5 } else { 9 };
}

fn scaled(flag: bool) -> usize {
    const LOCAL: usize = if cfg!(unix) { 3 } else { 6 };
    if flag { LOCAL + Buffer::SPAN } else { DIRECT + WIDTH }
}

fn main() {
    let buffer = Buffer([0; if cfg!(unix) { 4 } else { 8 }]);
    println!(
        "{} {} {} {}",
        scaled(true),
        scaled(false),
        Mode::Narrow as usize,
        buffer.0.len()
    );
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // The runtime `if` in `scaled` is still instrumented -- declining a
        // const initialiser must not decline the whole file.
        assert!(transformed.code.contains("::decision("));
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(limitation_kind_of)
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("rust-const-context-not-instrumented"));

        let original = compile_and_run(source, "const-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "const-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    /// An attributed `let` whose initializer is a decision: its probe block
    /// and the decision both wrap the initializer, and nest. They crossed,
    /// and the file was not instrumented at all.
    #[test]
    fn an_attributed_let_holding_a_decision_still_compiles() {
        let source = r#"fn main() {
    let a = std::env::args().count() > 5;
    #[allow(unused)]
    let kept = a && !a;
    #[allow(clippy::nonminimal_bool)]
    let either = a || !a;
    println!("{kept} {either}");
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert_eq!(transformed.code.matches("::decision(").count(), 2);
        let original = compile_and_run(source, "attributed-let-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "attributed-let-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
    }

    /// Labelled blocks, loops inside them and in closures, trailing
    /// asserting blocks, `const fn` control flow, an empty `match`, bracket
    /// macros and `?`: instrumented, the program builds and prints the same.
    #[test]
    fn control_flow_shapes_still_compile_and_run_the_same() {
        let source = r#"fn j(c: bool) -> u8 {
    'blk: {
        if c {
            break 'blk 1;
        }
        loop {
            break;
        }
        while c {
            break;
        }
        for _ in 0..1 {
            break;
        }
        let _f = || loop {
            break;
        };
        2
    }
}

fn k(xs: &[u8]) {
    for x in xs {
        assert!(*x > 0)
    }
    if xs.is_empty() {
        assert!(true)
    } else {
        assert_eq!(xs.len(), xs.len())
    }
}

const fn n(mut i: u8) -> u8 {
    while i < 3 && i != 9 {
        i += 1;
    }
    loop {
        if i > 1 {
            break;
        }
    }
    match i {
        0 => 1,
        _ if i > 2 => 2,
        _ => {
            3
        }
    }
}

#[allow(dead_code)]
fn o(n: std::convert::Infallible) -> u8 {
    match n {}
}

fn p(a: bool) -> u8 {
    let f = || -> u8 {
        if a { 1 } else { 2 }
    };
    let v = vec![f(), 2];
    let w = [f(); 2];
    println!("{:?} {:?}", v, w);
    #[rustfmt::skip]
    println!("{}", a);
    f()
}

fn q(a: Option<u8>) -> Option<u8> {
    let b = a?;
    let c = if let Some(x) = a { x } else { b };
    while let Some(y) = a {
        if y > c {
            break;
        }
        break;
    }
    Some(c)
}

const N: u8 = n(0);

fn main() {
    k(&[1, 2]);
    k(&[]);
    println!("{} {} {} {} {} {:?} {:?}", j(true), j(false), N, p(true), p(false), q(Some(3)), q(None));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let original = compile_and_run(source, "control-flow-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "control-flow-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert!(
            original.status.success(),
            "{}",
            String::from_utf8_lossy(&original.stderr)
        );
    }

    #[test]
    fn a_probed_global_allocator_would_recurse_into_itself() {
        // The runtime allocates, so a probe inside `alloc` re-enters `alloc` and
        // recurses until the stack is gone. bytes-1.12.1's
        // tests/test_bytes_odd_alloc.rs installs one of these, and the
        // instrumented binary died with SIGSEGV before libtest could list a
        // single test, while the uninstrumented binary listed them fine.
        let source = r#"use std::alloc::{GlobalAlloc, Layout, System};

struct Odd;

unsafe impl GlobalAlloc for Odd {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.align() == 1 && layout.size() > 0 {
            System.alloc(layout)
        } else {
            System.alloc(layout)
        }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        System.dealloc(pointer, layout);
    }
}

#[global_allocator]
static ODD: Odd = Odd;

fn classify(flag: bool) -> usize {
    if flag { 1 } else { 2 }
}

fn main() {
    let held = std::vec![7u8; 32];
    println!("{} {}", classify(!held.is_empty()), held.len());
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // Nothing inside the allocator may carry a probe...
        let allocator = transformed
            .code
            .split("unsafe impl GlobalAlloc for Odd")
            .nth(1)
            .and_then(|rest| rest.split("#[global_allocator]").next())
            .expect("the instrumented source still contains the allocator impl");
        assert!(
            !allocator.contains("__supercov_runtime_v1"),
            "probe injected into a GlobalAlloc impl:\n{allocator}"
        );
        // ...while `classify`, right next to it, is still measured.
        assert!(transformed.code.contains("::decision("));
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(limitation_kind_of)
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("rust-global-allocator-not-instrumented"));

        let original = compile_and_run(source, "alloc-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "alloc-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn match_arms_record_selection_without_changing_behavior() {
        let source = r#"#[derive(Debug)]
enum Shape { Dot, Line(i32), Box { w: i32, h: i32 } }

fn area(shape: &Shape) -> i32 {
    match shape {
        Shape::Dot => 0,
        Shape::Line(length) if *length < 0 => -length,
        Shape::Line(length) => *length,
        Shape::Box { w, h } => {
            let area = w * h;
            area
        }
    }
}

fn describe(value: i32) -> &'static str {
    let inner = |v: i32| match v { 0 => "none", 1 => "one", _ => "many" };
    match value {
        0 => inner(value),
        n if n < 0 => unsafe { std::hint::unreachable_unchecked() },
        n => match n % 2 {
            0 => "even",
            _ => inner(n),
        },
    }
}

fn main() {
    for shape in [Shape::Dot, Shape::Line(-3), Shape::Line(4), Shape::Box { w: 2, h: 5 }] {
        println!("{shape:?}={}", area(&shape));
    }
    for value in [0, 1, 3, 8] {
        println!("{value}:{}", describe(value));
    }
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert!(transformed.code.contains("::arms(__SUPERCOV_ARMS_"));
        assert_eq!(
            transformed.code.matches("const __SUPERCOV_ARMS_").count(),
            4
        );
        let arms = transformed
            .manifest
            .branches
            .iter()
            .filter(|branch| branch.kind == "match-arm")
            .count();
        assert_eq!(arms, 4 + 3 + 3 + 2);
        // Every arm's alternatives appear in a table, and the source keeps
        // its meaning.
        for branch in transformed
            .manifest
            .branches
            .iter()
            .filter(|branch| branch.kind == "match-arm")
        {
            for alternative in &branch.alternatives {
                let ordinal = transformed
                    .probes
                    .hits
                    .iter()
                    .position(|id| id == &alternative.id)
                    .unwrap_or_else(|| panic!("{} has no probe", alternative.id));
                assert!(
                    transformed.code.contains("&[u32] = &["),
                    "{} is not in any table",
                    alternative.id
                );
                assert!(transformed.code.contains(&format!("{ordinal}")));
            }
        }
        let original = compile_and_run(source, "original-arms");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented-arms",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    /// A loop that cannot be empty -- over a non-empty array literal, or a
    /// range between integer literals that holds a value, through the
    /// adapters that keep every item -- has only its entry to observe; one
    /// that may be empty keeps both outcomes.
    #[test]
    fn a_loop_that_cannot_be_empty_records_only_its_entry() {
        for (iterable, never_empty) in [
            ("[1, 2]", true),
            ("&[1, 2]", true),
            ("([1])", true),
            ("[1, 2].iter().enumerate()", true),
            ("[1].into_iter().map(|v| v * 2).rev()", true),
            ("0..3", true),
            ("1_000u32..1_001", true),
            ("0..=0", true),
            ("0..0", false),
            ("3..1", false),
            ("[0; 3]", false),
            ("[]", false),
            ("0x0..0x3", false),
            ("0..n", false),
            ("..3", false),
            ("values", false),
            ("values.iter()", false),
            ("[1].iter().skip(1)", false),
            ("text.split(',')", true),
            ("text.rsplit(',').enumerate()", true),
            ("values.split(|v| *v == 0)", true),
            ("text.splitn(2, ',')", true),
            ("text.rsplitn(1, ',')", true),
            ("text.splitn(0, ',')", false),
            ("text.splitn(n, ',')", false),
            ("text.lines()", false),
            ("text.split_whitespace()", false),
            ("KINDS", true),
            ("KINDS.iter()", true),
            ("ORDER", true),
            ("EMPTY", false),
            ("0..COUNT", true),
            ("1..=COUNT", true),
            ("COUNT..3", false),
            ("0..NO_COUNT", false),
            ("0..SUM", false),
            ("NONE", false),
            ("COPY", false),
            ("ALIAS", false),
            ("self::KINDS", false),
            // A constant's own value is read without following another.
            ("RANGE", false),
        ] {
            let source = format!(
                "const KINDS: &[&str] = &[\"a\", \"b\"];\nstatic ORDER: [u8; 2] = [1, 2];\nstatic EMPTY: [u8; 0] = [];\nconst COUNT: usize = 3;\nconst NO_COUNT: usize = 0;\nconst SUM: usize = 1 + 2;\nconst NONE: &[u8] = &[];\nconst COPY: [u8; 3] = [0; 3];\nconst ALIAS: &[&str] = KINDS;\nconst RANGE: std::ops::Range<usize> = 0..COUNT;\nfn f(values: &[i32], n: usize, text: &str) {{ for _ in {iterable} {{}} }}"
            );
            let transformed =
                instrument_rust_source("src/lib.rs", &source, "crate::__supercov_runtime_v1")
                    .unwrap();
            let branch = transformed
                .manifest
                .branches
                .iter()
                .find(|branch| branch.kind == "for-loop")
                .unwrap();
            let labels = branch
                .alternatives
                .iter()
                .map(|alternative| alternative.label.as_str())
                .collect::<Vec<_>>();
            let expected: &[&str] = if never_empty {
                &["entered"]
            } else {
                &["zero iterations", "entered"]
            };
            assert_eq!(labels, expected, "{iterable}");
            // The adapter is given an ordinal for each outcome it may report.
            assert!(transformed.code.contains("::for_loop(("), "{iterable}");
        }
    }

    #[test]
    fn loops_logic_and_try_record_their_branches_without_changing_behavior() {
        let source = r#"use std::ops::ControlFlow;

fn total(values: &[i32]) -> i32 {
    let mut sum = 0;
    for value in values {
        sum += value;
    }
    'outer: for row in 0..3 {
        for column in 0..3 {
            if column > row {
                continue 'outer;
            }
            sum += row * column;
        }
    }
    sum
}

fn first_even(values: &[i32]) -> Option<i32> {
    let mut index = 0;
    'scan: while index < values.len() {
        if values[index] % 2 == 0 {
            break 'scan;
        }
        index += 1;
    }
    let mut it = values.iter().skip(index);
    while let Some(value) = it.next() {
        return Some(*value);
    }
    None
}

fn parse_twice(text: &str) -> Result<i32, String> {
    let value: i32 = text.trim().parse().map_err(|_| "bad".to_string())?;
    let doubled = Some(value).map(|v| v * 2).ok_or("none")?;
    Ok(doubled)
}

fn halve(value: i32) -> Option<i32> {
    let even = (value % 2 == 0).then_some(value)?;
    Some(even / 2)
}

fn flow(values: &[i32]) -> ControlFlow<i32, i32> {
    let mut sum = 0;
    for value in values {
        let step: ControlFlow<i32, i32> = if *value < 0 { ControlFlow::Break(*value) } else { ControlFlow::Continue(*value) };
        sum += step?;
    }
    ControlFlow::Continue(sum)
}

fn gate(a: bool, b: bool, c: bool) -> bool {
    let both = a && b;
    let either = a || b || c;
    both || (either && !c) || (c && a && (b || !b))
}

fn main() {
    println!("{} {}", total(&[]), total(&[1, 2, 3]));
    println!("{:?} {:?} {:?}", first_even(&[]), first_even(&[1, 3]), first_even(&[1, 4, 6]));
    println!("{:?} {:?}", parse_twice(" 21 "), parse_twice("x"));
    println!("{:?} {:?}", halve(8), halve(7));
    println!("{:?} {:?}", flow(&[1, 2]), flow(&[1, -5, 2]));
    for a in [false, true] {
        for b in [false, true] {
            for c in [false, true] {
                print!("{}", gate(a, b, c) as u8);
            }
        }
    }
    println!();
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        for marker in [
            "::logical((",
            "::for_loop((",
            "::entered(&mut __supercov_loop_",
            "::zero_iterations(__supercov_loop_",
            "::TryProbe::probe((",
        ] {
            assert!(transformed.code.contains(marker), "{marker} missing");
        }
        let kinds = |kind: &str| {
            transformed
                .manifest
                .branches
                .iter()
                .filter(|branch| branch.kind == kind)
                .count()
        };
        assert_eq!(kinds("for-loop"), 3 + 1 + 3);
        assert_eq!(kinds("while-loop"), 2);
        assert_eq!(kinds("try-operator"), 4);
        assert_eq!(kinds("logical-and"), 4);
        assert_eq!(kinds("logical-or"), 5);
        assert!(!transformed.manifest.limitations.iter().any(|limitation| {
            limitation.get("id").and_then(|id| id.as_str())
                == Some("rust-structural-branch-probes-not-yet-injected")
        }));
        let original = compile_and_run(source, "original-structural");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "instrumented-structural",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn cfg_gated_sibling_blocks_keep_their_tail_position() {
        // memchr's is_available returns bool from one of two cfg-gated blocks.
        // A probe PREPENDED to the second block sits between the siblings,
        // survives the cfg strip, and becomes the new `()` tail -- 32 E0308s
        // across the crate. Attributed blocks take the probe inside their
        // braces instead, where the same cfg governs both.
        let source = r#"pub fn is_available() -> bool {
    #[cfg(target_endian = "little")]
    {
        true
    }
    #[cfg(not(target_endian = "little"))]
    {
        false
    }
}

fn main() {
    println!("{}", is_available());
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        let original = compile_and_run(source, "cfg-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "cfg-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        // The kept block is still probed -- inside its braces.
        assert!(
            transformed
                .code
                .contains("{crate::__supercov_runtime_v1::hit("),
            "{}",
            transformed.code
        );

        // An attributed `let` takes the probe inside its initializer, an
        // attributed expression statement inside a block; a `let` without an
        // initializer is the one shape left declared.
        let attributed_let = r#"fn main() {
    #[cfg(target_endian = "little")]
    let value = 1;
    #[cfg(not(target_endian = "little"))]
    let value = 2;
    #[cfg(target_endian = "little")]
    let borrowed: &String = &String::from("little");
    #[cfg(not(target_endian = "little"))]
    let borrowed: &String = &String::from("big");
    #[cfg(target_endian = "little")]
    print!("le ");
    #[cfg(not(target_endian = "little"))]
    print!("be ");
    #[allow(unused_assignments)]
    let mut later;
    later = value + 1;
    println!("{value} {borrowed} {later}");
}
"#;
        let transformed = instrument_rust_source(
            "src/main.rs",
            attributed_let,
            "crate::__supercov_runtime_v1",
        )
        .unwrap();
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(limitation_kind_of)
            .collect::<BTreeSet<_>>();
        // Declared only for `let mut later;`.
        assert!(ids.contains("rust-attributed-statement-probes-not-injected"));
        assert!(
            transformed
                .code
                .contains("let value =  { crate::__supercov_runtime_v1::hit(")
        );
        assert!(
            transformed
                .code
                .contains("let borrowed: &String =  { crate::__supercov_runtime_v1::hit(")
        );
        assert!(
            transformed
                .code
                .contains("] { crate::__supercov_runtime_v1::hit(")
        );
        let original = compile_and_run(attributed_let, "cfg-let-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "cfg-let-instrumented",
        );
        assert_eq!(instrumented.status, original.status);
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);

        // A brace macro closing a block is a statement to rustc, so its
        // attributes are legal where an attributed tail expression's are not
        // (tokio: `#[rustfmt::skip] tokio::select! { .. }`). The probe goes
        // before the attributes; a `cfg` there stays declared.
        let trailing_macro = r#"macro_rules! pick { ($e:expr) => { $e } }
fn value() -> i32 {
    let base = 20;
    #[rustfmt::skip]
    pick! { base + 1 }
}
fn effect() {
    #[rustfmt::skip]
    println! { "effect" }
}
fn gated() {
    #[cfg(target_endian = "little")]
    println! { "little" }
}
fn main() {
    effect();
    gated();
    println!("{}", value());
}
"#;
        let transformed = instrument_rust_source(
            "src/main.rs",
            trailing_macro,
            "crate::__supercov_runtime_v1",
        )
        .unwrap();
        assert!(
            transformed
                .code
                .contains("{ crate::__supercov_runtime_v1::hit(")
        );
        assert!(
            transformed
                .code
                .contains("); #[rustfmt::skip]\n    pick! { base + 1 } }")
        );
        assert!(
            transformed
                .code
                .contains("); #[rustfmt::skip]\n    println! { \"effect\" } }")
        );
        assert!(
            transformed.code.contains(
                "\n    #[cfg(target_endian = \"little\")]\n    println! { \"little\" }\n"
            )
        );
        let ids = transformed
            .manifest
            .limitations
            .iter()
            .filter_map(limitation_kind_of)
            .collect::<BTreeSet<_>>();
        assert!(ids.contains("rust-attributed-statement-probes-not-injected"));
        let original = compile_and_run(trailing_macro, "trailing-macro-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "trailing-macro-instrumented",
        );
        assert_eq!(
            instrumented.status,
            original.status,
            "{}",
            String::from_utf8_lossy(&instrumented.stderr)
        );
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn an_attributed_macro_statement_keeps_its_macro_in_statement_position() {
        // hyper's `trace!` expands to `#[cfg(feature = "tracing")] { .. }`,
        // which is legal only where the expansion is a statement. Wrapping
        // the call as `#[cfg(..)] { hit; (trace!("..")) }` made it an
        // attributed expression, which is unstable, and hyper did not build.
        let source = r#"macro_rules! trace {
    ($($arg:tt)*) => {
        #[cfg(target_endian = "little")]
        {
            println!($($arg)+);
        }
    }
}
fn manual() {
    #[cfg(any(target_endian = "little", target_endian = "big"))]
    trace!("manual");
    let _ = 1;
}
fn main() {
    manual();
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // The probe is inside the block, ahead of the macro, and the macro
        // keeps its semicolon.
        assert!(
            transformed.code.contains(r#"trace!("manual"); }"#),
            "{}",
            transformed.code
        );
        let original = compile_and_run(source, "attributed-macro-statement-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "attributed-macro-statement-instrumented",
        );
        assert_eq!(
            instrumented.status,
            original.status,
            "{}",
            String::from_utf8_lossy(&instrumented.stderr)
        );
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stderr, original.stderr);
    }

    #[test]
    fn an_attributed_lets_macro_initialiser_keeps_its_value() {
        // The rule that keeps an attributed macro STATEMENT in statement
        // position must not reach a `let` initialiser: tokio's
        // `#[cfg(..)] let coop = ready!(..);` became `let coop = { hit;
        // ready!(..); };`, which is `()`, and the next line called a method
        // on it.
        let source = r#"macro_rules! first {
    ($e:expr) => { $e }
}
fn value(flag: bool) -> i32 {
    #[cfg(any(target_endian = "little", target_endian = "big"))]
    let chosen = first!(if flag { 7 } else { 3 });
    chosen + 1
}
fn main() {
    println!("{}", value(true));
}
"#;
        let transformed =
            instrument_rust_source("src/main.rs", source, "crate::__supercov_runtime_v1").unwrap();
        // The initialiser keeps its value: the block ends in the macro, with
        // no semicolon to discard it.
        assert!(
            !transformed
                .code
                .contains("first!(if flag { 7 } else { 3 }); }"),
            "{}",
            transformed.code
        );
        let original = compile_and_run(source, "attributed-let-macro-original");
        let instrumented = compile_and_run(
            &format!("{}\n{NOOP_RUNTIME}", transformed.code),
            "attributed-let-macro-instrumented",
        );
        assert_eq!(
            instrumented.status,
            original.status,
            "{}",
            String::from_utf8_lossy(&instrumented.stderr)
        );
        assert_eq!(instrumented.stdout, original.stdout);
        assert_eq!(instrumented.stdout, b"8\n");
    }

    #[test]
    fn obligations_no_probe_can_reach_are_declined() {
        // A `const fn` body has no runtime to record into, and a
        // `GlobalAlloc` implementation would probe the allocator its probe
        // allocates in. Both were declared and still counted, so smallvec's
        // `TaggedLen` -- four `const fn` methods -- read 0% covered where
        // the independent LLVM coverage oracle reads 89%.
        let source = r#"pub struct Tagged(usize);
impl Tagged {
    pub const fn new(len: usize, on_heap: bool) -> Self {
        Self(if on_heap { len << 1 } else { len })
    }
    pub fn plain(len: usize) -> Self {
        Self(if len > 0 { len } else { 0 })
    }
}
"#;
        let manifest = build_rust_manifest("src/lib.rs", source).unwrap();
        let declined = manifest.unmeasured.iter().collect::<BTreeSet<_>>();
        assert!(!declined.is_empty(), "the const fn was not declined");

        // Everything the const fn holds is declined; the plain one is not.
        let line_of = |id: &str| {
            manifest
                .points
                .iter()
                .find(|point| point.id == id)
                .map(|point| point.line)
                .or_else(|| {
                    manifest
                        .decisions
                        .iter()
                        .find(|decision| decision.id == id)
                        .map(|decision| decision.line)
                })
                .or_else(|| {
                    manifest
                        .branches
                        .iter()
                        .find(|branch| branch.id == id)
                        .map(|branch| branch.line)
                })
        };
        for id in &declined {
            let line = line_of(id).unwrap_or_else(|| panic!("no obligation {id}"));
            assert!(
                (3..=5).contains(&line),
                "declined an obligation outside the const fn, at line {line}"
            );
        }
        let measured = manifest
            .points
            .iter()
            .map(|point| (point.id.clone(), point.line))
            .filter(|(id, _)| !declined.contains(id))
            .collect::<Vec<_>>();
        assert!(
            measured.iter().any(|(_, line)| (6..=8).contains(line)),
            "the plain fn must stay measured: {measured:?}"
        );
    }

    #[test]
    fn a_documented_function_is_reported_where_it_starts() {
        // itertools' `group_by` was reported at the line of its doc comment,
        // three lines above the `fn`, because the node's range begins there.
        let source = r#"/// Documented.
/// Twice.
#[inline]
pub fn documented(value: i32) -> i32 {
    value + 1
}
"#;
        let manifest = build_rust_manifest("src/lib.rs", source).unwrap();
        let point = manifest
            .points
            .iter()
            .find(|point| {
                point.kind == PointKind::Function && point.label.as_deref() == Some("documented")
            })
            .unwrap();
        assert_eq!(point.line, 4, "{point:?}");
        assert!(
            point.source.starts_with("pub fn documented"),
            "{}",
            point.source
        );
    }

    #[test]
    fn rejects_non_crate_local_runtime_paths() {
        assert_eq!(
            instrument_rust_source("src/lib.rs", "fn okay() {}", "supercov::runtime"),
            Err(RustInstrumenterError::InvalidRuntimePath)
        );
    }

    #[test]
    fn rejects_invalid_rust_without_partial_obligations() {
        assert!(matches!(
            build_rust_manifest("src/lib.rs", "fn broken( {\n"),
            Err(RustInstrumenterError::Parse(_))
        ));
    }

    /// The cfg of an x86_64 Linux target, as `rustc --print cfg` reports it.
    fn linux() -> TargetCfg {
        TargetCfg::new(
            &[
                "unix",
                "target_os=\"linux\"",
                "target_family=\"unix\"",
                "target_pointer_width=\"64\"",
            ]
            .map(|value| value.parse::<cargo_platform::Cfg>().unwrap()),
        )
    }

    /// A known build settles `test`, `debug_assertions` and its features as
    /// the target settles its own keys; anything else stays for the build to
    /// answer.
    #[test]
    fn a_known_build_settles_test_debug_assertions_and_features() {
        let wasm = TargetCfg::new(&["target_arch=\"wasm32\"".parse().unwrap()]);
        let evaluate = |cfg: &TargetCfg, predicate: &str| cfg.evaluate(&predicate.parse().unwrap());
        assert_eq!(evaluate(&wasm, "feature = \"audit\""), None);
        assert_eq!(evaluate(&wasm, "test"), None);
        let build = wasm.for_build(false, true, BTreeSet::from(["audit".to_owned()]));
        assert_eq!(evaluate(&build, "target_arch = \"wasm32\""), Some(true));
        assert_eq!(evaluate(&build, "test"), Some(false));
        assert_eq!(evaluate(&build, "debug_assertions"), Some(true));
        assert_eq!(evaluate(&build, "feature = \"audit\""), Some(true));
        assert_eq!(evaluate(&build, "feature = \"other\""), Some(false));
        assert_eq!(evaluate(&build, "custom"), None);
        assert_eq!(evaluate(&build, "custom = \"value\""), None);
    }

    fn limitation_kinds(manifest: &CoverageManifest) -> BTreeSet<String> {
        manifest
            .limitations
            .iter()
            .filter_map(limitation_kind_of)
            .map(str::to_owned)
            .collect()
    }

    #[test]
    fn an_insertion_past_the_source_or_inside_a_character_is_refused() {
        let insert = |offset| {
            let mut insertions = Vec::new();
            push_direct(&mut insertions, offset, "x".into());
            apply_insertions("aé", insertions)
        };
        assert_eq!(insert(1), Ok("axé".into()));
        assert_eq!(insert(3), Ok("aéx".into()));
        assert_eq!(insert(4), Err(RustInstrumenterError::InvalidRange));
        // `é` is the two bytes 1..3.
        assert_eq!(insert(2), Err(RustInstrumenterError::InvalidRange));
    }

    #[test]
    fn a_range_past_the_source_or_inside_a_character_has_no_location() {
        let locations = SourceLocations::new("aé");
        let range =
            |start: u32, end: u32| locations.range(TextRange::new(start.into(), end.into()));
        assert_eq!(range(0, 3), Ok((0, 3)));
        assert_eq!(range(0, 4), Err(RustInstrumenterError::InvalidRange));
        assert_eq!(range(2, 3), Err(RustInstrumenterError::InvalidRange));
        assert_eq!(range(0, 2), Err(RustInstrumenterError::InvalidRange));
    }

    #[test]
    fn an_obligation_recorded_twice_is_kept_once() {
        let source = "fn f(a: bool, b: bool) { if a && b {} }";
        let tree = SourceFile::parse(source, Edition::Edition2024).tree();
        let condition = tree
            .syntax()
            .descendants()
            .find_map(ast::IfExpr::cast)
            .and_then(|expression| expression.condition())
            .unwrap();
        let range = condition.syntax().text_range();
        let mut collector = RustObligationCollector::new("src/lib.rs", source);
        for _ in 0..2 {
            collector.point(range, PointKind::Statement, None);
            collector.decision(&condition, "if");
            collector.branch(range, "for-loop", [("zero", "zero iterations")]);
        }
        let manifest = collector.manifest;
        assert_eq!(manifest.points.len(), 1);
        assert_eq!(manifest.decisions.len(), 1);
        assert_eq!(manifest.decisions[0].conditions, ["a", "b"]);
        // The decision's outcome and the loop.
        assert_eq!(manifest.branches.len(), 2);
    }

    /// Instrumentation never sees a tree with errors, but the collector can
    /// be handed one: what is missing is passed over, and an operator short
    /// of an operand is one condition.
    #[test]
    fn a_tree_with_errors_yields_obligations_only_where_it_is_whole() {
        let source = concat!(
            "fn f() { if }\n",
            "fn g() { while }\n",
            "fn h(x: u8) { match x { _ if => 1 } }\n",
            "fn i(a: bool) { if (;) {} let _ = (a &&) || a; }\n",
        );
        let tree = SourceFile::parse(source, Edition::Edition2024);
        assert!(!tree.errors().is_empty());
        let manifest = RustObligationCollector::new("src/lib.rs", source)
            .collect(&tree.tree(), &[], &[])
            .unwrap();
        let decisions = manifest
            .decisions
            .iter()
            .map(|decision| (decision.kind.as_str(), decision.conditions.clone()))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            decisions,
            BTreeSet::from([
                ("if", vec!["(;)".to_owned()]),
                ("value", vec!["a &&".to_owned(), "a".to_owned()]),
            ])
        );
        // The `while` with no condition is still a loop.
        assert!(
            manifest
                .branches
                .iter()
                .any(|branch| branch.kind == "while-loop" && branch.line == 2)
        );
    }

    /// A tree collected against its source cut short anywhere is refused
    /// whole, wherever its first range falls past the end.
    #[test]
    fn a_tree_collected_against_any_shorter_source_is_refused_whole() {
        let source = concat!(
            "pub fn f(a: bool, b: bool, v: &[u8]) -> Option<u8> {\n",
            "    if a && (b || !a) { return None; }\n",
            "    for x in v { if *x > 1 { break; } }\n",
            "    while a { break; }\n",
            "    let n = match v.len() { 0 => 1, n if n > 2 => 2, _ => 3 };\n",
            "    let g = |y: u8| y.checked_add(1)?.checked_sub(1);\n",
            "    let w = if let Some(z) = v.first() && *z > 0 { *z } else { 0 };\n",
            "    assert!(a || b, \"a or b\");\n",
            "    Some(g(n)? + w)\n",
            "}\n",
        );
        let tree = SourceFile::parse(source, Edition::Edition2024);
        assert!(tree.errors().is_empty(), "{:?}", tree.errors());
        let tree = tree.tree();
        assert!(
            RustObligationCollector::new("src/lib.rs", source)
                .collect(&tree, &[], &[])
                .is_ok()
        );
        for cut in 0..source.trim_end().len() {
            assert_eq!(
                RustObligationCollector::new("src/lib.rs", &source[..cut]).collect(&tree, &[], &[]),
                Err(RustInstrumenterError::InvalidRange),
                "cut at {cut}"
            );
        }
    }

    /// The same tree collected against a source of its length with two
    /// bytes made one `é`, at every place: only the ranges with an edge
    /// inside it fall within a character, and each such source is refused
    /// whole, never half collected and never with a panic.
    #[test]
    fn a_tree_collected_against_a_source_with_a_character_anywhere_is_refused_whole() {
        let source = concat!(
            "pub fn f(a: bool, b: bool, v: &[u8]) -> Option<u8> {\n",
            "    if a && (b || !a) { return None; }\n",
            "    for x in v { if *x > 1 { break; } }\n",
            "    while a { break; }\n",
            "    let n = match v.len() { 0 => 1, n if n > 2 => 2, _ => 3 };\n",
            "    let g = |y: u8| y.checked_add(1)?.checked_sub(1);\n",
            "    let w = if let Some(z) = v.first() && *z > 0 { *z } else { 0 };\n",
            "    assert!(a || b, \"a or b\");\n",
            "    Some(g(n)? + w)\n",
            "}\n",
        );
        let tree = SourceFile::parse(source, Edition::Edition2024).tree();
        let mut refused = 0;
        for at in 0..source.len() - 1 {
            let mut shifted = source.to_owned();
            shifted.replace_range(at..at + 2, "é");
            match RustObligationCollector::new("src/lib.rs", &shifted).collect(&tree, &[], &[]) {
                Ok(_) => {}
                Err(error) => {
                    assert_eq!(error, RustInstrumenterError::InvalidRange, "at {at}");
                    refused += 1;
                }
            }
        }
        assert!(refused > 10, "{refused}");
    }

    /// A match of no arm -- on a type with no value -- has no arm to
    /// record.
    #[test]
    fn a_match_of_no_arm_records_no_arm() {
        let source = "enum Never {}\nfn f(x: Never) -> u8 { match x {} }\n";
        let tree = SourceFile::parse(source, Edition::Edition2024).tree();
        let manifest = RustObligationCollector::new("src/lib.rs", source)
            .collect(&tree, &[], &[])
            .unwrap();
        assert!(
            manifest
                .branches
                .iter()
                .all(|branch| branch.kind != "match-arm")
        );
    }

    #[test]
    fn a_collector_given_a_tree_of_other_text_reports_an_invalid_range() {
        let tree = SourceFile::parse("fn f() { work(); }", Edition::Edition2024).tree();
        assert_eq!(
            RustObligationCollector::new("src/lib.rs", "fn f() {}").collect(&tree, &[], &[]),
            Err(RustInstrumenterError::InvalidRange)
        );
    }

    #[test]
    fn a_range_that_is_no_argument_list_or_call_names_no_condition() {
        let tree = SourceFile::parse("fn f(a: bool) { g(a) }", Edition::Edition2024).tree();
        let root = tree.syntax();
        let function = root.descendants().find_map(ast::Fn::cast).unwrap();
        let range = function.syntax().text_range();
        assert!(assertion_condition(root, range).is_none());
        assert_eq!(assertion_argument_count(root, range), 0);
        assert!(standalone_matches(root, &[range], &[]).is_empty());
        let arguments = root
            .descendants()
            .find_map(ast::ArgList::cast)
            .unwrap()
            .syntax()
            .text_range();
        assert_eq!(
            assertion_condition(root, arguments).map(|condition| condition.to_string()),
            Some("a".into())
        );
        assert_eq!(assertion_argument_count(root, arguments), 1);
    }

    #[test]
    fn an_assertion_with_no_arguments_is_no_decision() {
        let manifest = build_rust_manifest("src/lib.rs", "pub fn f() { assert!(); }\n").unwrap();
        assert!(manifest.decisions.is_empty(), "{:?}", manifest.decisions);
    }

    #[test]
    fn code_the_target_leaves_out_raises_no_limitation_and_no_gate() {
        let source = concat!(
            "#[cfg(windows)]\n",
            "pub const fn one() -> u8 { 1 }\n",
            "#[cfg(windows)]\n",
            "mod w {\n",
            "    #[cfg(feature = \"x\")]\n",
            "    pub fn two() -> u8 { 2 }\n",
            "}\n",
        );
        let runtime = "crate::__supercov_runtime_v1";
        let linux =
            instrument_rust_file("src/lib.rs", source, runtime, false, Some(&linux())).unwrap();
        assert!(linux.manifest.limitations.is_empty());
        assert!(linux.probes.cfgs.is_empty(), "{:?}", linux.probes.cfgs);
        // Any target: the const fn is declared and both gates are open.
        let any = instrument_rust_file("src/lib.rs", source, runtime, false, None).unwrap();
        assert!(limitation_kinds(&any.manifest).contains("rust-const-context-not-instrumented"));
        let predicates = any
            .probes
            .cfgs
            .iter()
            .map(|gate| gate.predicate.as_str())
            .collect::<BTreeSet<_>>();
        assert!(predicates.contains("feature = \"x\""), "{predicates:?}");
    }

    #[test]
    fn a_gate_around_test_code_holds_only_the_obligations_that_are_measured() {
        let source = concat!(
            "#[cfg(feature = \"x\")]\n",
            "mod m {\n",
            "    pub fn g() -> u8 { 1 }\n",
            "    #[cfg(test)]\n",
            "    mod tests {\n",
            "        fn t() { let _ = super::g(); }\n",
            "    }\n",
            "}\n",
        );
        let instrumented = instrument_rust_file(
            "src/lib.rs",
            source,
            "crate::__supercov_runtime_v1",
            false,
            Some(&linux()),
        )
        .unwrap();
        let [gate] = instrumented.probes.cfgs.as_slice() else {
            panic!("{:?}", instrumented.probes.cfgs);
        };
        let measured = instrumented
            .manifest
            .points
            .iter()
            .map(|point| point.id.clone())
            .collect::<BTreeSet<_>>();
        assert_eq!(measured.len(), 2);
        assert_eq!(
            gate.obligations.iter().cloned().collect::<BTreeSet<_>>(),
            measured
        );
    }

    #[test]
    fn only_a_break_with_no_loop_or_closure_between_leaves_the_loop() {
        let tree = SourceFile::parse(
            concat!(
                "fn f(c: bool) {\n",
                "    while c {\n",
                "        break;\n",
                "        loop { break; }\n",
                "        while c { break; }\n",
                "        for _ in 0..1 { break; }\n",
                "        let _ = || { break; };\n",
                "        'o: loop { break 'o; }\n",
                "    }\n",
                "}\n",
            ),
            Edition::Edition2024,
        )
        .tree();
        let body = tree
            .syntax()
            .descendants()
            .find_map(ast::WhileExpr::cast)
            .and_then(|expression| expression.loop_body())
            .unwrap();
        let first = body
            .syntax()
            .descendants()
            .find_map(ast::BreakExpr::cast)
            .unwrap()
            .syntax()
            .text_range();
        assert_eq!(own_breaks(&body, None), [first]);
    }

    #[test]
    fn a_line_with_only_trivia_before_it_keeps_its_offset() {
        let source = "// note\nfn f() {}\n";
        let tree = SourceFile::parse(source, Edition::Edition2024).tree();
        assert_eq!(before_its_line(tree.syntax(), source, 8), 8);
    }

    #[test]
    fn a_block_with_no_brace_has_no_entry() {
        let tree = SourceFile::parse("fn f() { let _ = try; }", Edition::Edition2021).tree();
        let block = tree
            .syntax()
            .descendants()
            .filter_map(ast::BlockExpr::cast)
            .find(|block| block.stmt_list().is_none())
            .unwrap();
        assert_eq!(block_entry_offset(&block), None);
        assert_eq!(
            block_entry(&block),
            Err(RustInstrumenterError::InvalidRange)
        );
        let tree = SourceFile::parse("fn f() { #![allow(x)] }", Edition::Edition2021).tree();
        let body = tree
            .syntax()
            .descendants()
            .find_map(ast::BlockExpr::cast)
            .unwrap();
        assert_eq!(block_entry(&body), Ok(21));
    }

    #[test]
    fn a_macro_is_a_statement_only_inside_a_block() {
        let tree = SourceFile::parse(
            "vec![1; 2];\nfn f() { vec![1; 2]; }\nfn g() -> Vec<u8> { vec![1; 2] }\n",
            Edition::Edition2024,
        )
        .tree();
        let statements = tree
            .syntax()
            .descendants()
            .filter_map(ast::MacroCall::cast)
            .map(|call| is_statement_macro(&call))
            .collect::<Vec<_>>();
        assert_eq!(statements, [false, true, true]);
    }

    /// What the expression view leaves as written: a `vec!` whose arguments
    /// are neither an argument list nor an array, and one that forms a
    /// statement; and what it rewrites in place, line breaks kept.
    #[test]
    fn the_expression_view_keeps_every_line_where_it_was() {
        let view = |source: &str| expression_view(source, Edition::Edition2024).text;
        for kept in [
            "fn f() -> Vec<u8> { let v = vec!(0; 3); v }\n",
            "fn f() -> Vec<u8> { let v = vec![1 => 2]; v }\n",
            "fn f() { vec![0; 3]; }\n",
        ] {
            assert_eq!(view(kept), kept);
        }
        assert_eq!(
            view("fn f() -> Vec<u8> {\n    let v = vec!\n        [0; 3];\n    v\n}\n"),
            "fn f() -> Vec<u8> {\n    let v = &   \n        [0; 3];\n    v\n}\n"
        );
        assert_eq!(
            view("fn f(x: Option<u8>) -> bool {\n    matches!(x,\n        Some(_))\n}\n"),
            "fn f(x: Option<u8>) -> bool {\n    matches_(x,\n               )\n}\n"
        );
    }

    /// A `vec!` among the items reads as an array nowhere an array can
    /// stand, so the view does not parse: the original tree is used, and its
    /// macros stay declared.
    #[test]
    fn a_view_that_does_not_parse_leaves_the_original_tree() {
        let source = "vec![1; 2];\nfn f() { assert!(true); }\n";
        let parsed = parse_for_instrumentation(source).unwrap();
        assert_eq!(parsed.tree.syntax().to_string(), source);
        assert!(parsed.assertions.is_empty());
    }

    #[test]
    fn an_attributed_tail_takes_its_probe_inside_its_attributes() {
        let instrumented = instrument_rust_source(
            "src/lib.rs",
            "pub fn f(c: bool) -> i32 {\n    #[allow(unused_parens)] if c { 1 } else { 2 }\n}\n",
            "crate::__supercov_runtime_v1",
        )
        .unwrap();
        assert!(
            instrumented
                .code
                .contains("#[allow(unused_parens)] { crate::__supercov_runtime_v1::hit("),
            "{}",
            instrumented.code
        );
        // A `cfg_attr` on a trailing macro is a `cfg` too: no probe goes
        // where it might strip the statement and keep the probe.
        let declared = instrument_rust_source(
            "src/lib.rs",
            "pub fn f() -> u8 {\n    #[cfg_attr(test, allow(unused))] m! { 1 }\n}\n",
            "crate::__supercov_runtime_v1",
        )
        .unwrap();
        assert!(
            limitation_kinds(&declared.manifest)
                .contains("rust-attributed-statement-probes-not-injected")
        );
    }

    #[test]
    fn nothing_in_a_const_context_takes_a_probe() {
        let source = concat!(
            "pub const fn f(n: u8) -> u8 {\n",
            "    let mut i = n;\n",
            "    while i > 0 { i -= 1; }\n",
            "    while let Some(_) = None::<u8> {}\n",
            "    i\n",
            "}\n",
            "pub const fn g(o: Option<u8>) -> Option<u8> {\n",
            "    let v = o?;\n",
            "    Some(v)\n",
            "}\n",
            "const C: fn() -> u8 = || 1;\n",
            "const _: () = assert!(f(1) == 0);\n",
        );
        let instrumented =
            instrument_rust_source("src/lib.rs", source, "crate::__supercov_runtime_v1").unwrap();
        assert_eq!(instrumented.code, source);
        assert!(
            limitation_kinds(&instrumented.manifest)
                .contains("rust-const-context-not-instrumented")
        );
    }

    #[test]
    fn an_attributed_let_with_nothing_to_probe_is_declared_only_outside_test_code() {
        let source = "pub fn f() {\n    #[allow(unused)]\n    let x: u8;\n}\n";
        let runtime = "crate::__supercov_runtime_v1";
        let product = instrument_rust_source("src/lib.rs", source, runtime).unwrap();
        assert!(
            limitation_kinds(&product.manifest)
                .contains("rust-attributed-statement-probes-not-injected")
        );
        let test = instrument_rust_source("tests/it.rs", source, runtime).unwrap();
        assert!(test.manifest.limitations.is_empty());
    }

    #[test]
    fn a_transformation_that_no_longer_parses_is_written_out_only_when_asked() {
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-dump-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        assert_eq!(
            still_parses(
                "src/lib.rs",
                "fn f() {}".into(),
                Edition::Edition2024,
                || { unreachable!("nothing is written for text that parses") }
            ),
            Ok("fn f() {}".into())
        );
        let broken = || "fn f( {".to_owned();
        assert!(matches!(
            still_parses("src/a/lib.rs", broken(), Edition::Edition2024, || None),
            Err(RustInstrumenterError::Parse(_))
        ));
        assert!(!root.exists());
        assert!(matches!(
            still_parses("src/a/lib.rs", broken(), Edition::Edition2024, || {
                Some(root.clone().into_os_string())
            }),
            Err(RustInstrumenterError::Parse(_))
        ));
        assert_eq!(
            fs::read_to_string(root.join("src__a__lib.rs")).unwrap(),
            broken()
        );
        fs::remove_dir_all(root).unwrap();
    }
}
