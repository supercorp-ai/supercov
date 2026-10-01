//! Assertions coverage: which executed statements a test would catch breaking.
//!
//! A statement counts as asserted when changing it makes at least one passing
//! test that executes it fail. The change follows the statement: an `if` has its
//! condition inverted (each direction), `return x` returns undefined, a
//! declaration's value becomes undefined, an expression point evaluates to
//! undefined, and any other statement is skipped. Imports, declarations without
//! a value, and function, class and type declarations are not assessed.
//!
//! Supercov does not decide this itself. For each test it renders what the test
//! ran (its code, its helpers, and the source it executed) and asks Jev, one
//! yes/no question per statement, whether the test would fail. This module owns
//! the population, the change rules and the rendering; the CLI owns the requests,
//! the answer cache and the output. What Jev sees for one statement and one test
//! is also the cache key, so an answer is reused only when the question would be
//! asked with exactly the same text.
use crate::coverage_analysis::PointKind;
use crate::coverage_report::CoverageReport;
use oxc_allocator::Allocator;
use oxc_ast::ast::{Declaration, ExportDefaultDeclarationKind, Statement as JsStatement};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use oxc_span::{GetSpan, SourceType};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

mod go;
mod jvm;
mod ruby;
mod rust_source;

/// Statements starting on each line: their text and change (`None`: not
/// assessed).
type Starts = BTreeMap<usize, Vec<(String, Option<Change>)>>;

pub const MODEL: &str = "jev-1.13.0";
/// Tests asked about a statement before it may be called unchecked, then the
/// wider set asked before it is (measured: more tests remove false "unchecked"
/// flags, and 25 tests was no better than 15).
pub const FIRST_TESTS: usize = 5;
pub const MORE_TESTS: usize = 15;
pub const MAX_QUESTIONS: usize = 150;
/// A statement is asserted when some test's answer is at least this.
pub const THRESHOLD: f64 = 0.5;
const HEAD_LINES: usize = 10;
const HELPER_CHARS: usize = 3000;
/// The budget for data files a test names (Ruby spec support fixtures),
/// beside the helpers': the cases a data-driven test checks are its asserts.
const FIXTURE_CHARS: usize = 12000;
const CONTEXT_LINES: usize = 2;
const WINDOW_LINES: usize = 8;
const CODE_CHARS: usize = 40000;
const BODY_LINES: usize = 150;

pub const INSTRUCTIONS: &str = "Each question describes exactly one change to the program, made alone. The test passes on the code as written and executes the changed line. Question: with that change, does this test fail (an assertion fails, test or helper code throws, the test times out, or the process crashes)? Follow what the changed line computes to what the test checks; do not guess from names. In `code_run`, lines marked ▶ start statements this test ran.";

fn runtime(runner: &str) -> &'static str {
    match runner {
        "node:test" => {
            "node:test; a test past its timeout fails; after hooks still run. The tests spawn the program as child processes; an unhandled promise rejection ends a Node process."
        }
        "jest" => {
            "Jest; a test past its timeout (default 5000 ms) fails; an unhandled error during the run fails it; jest.fn() returns undefined unless given an implementation."
        }
        "pytest" => {
            "pytest; a failing assert or an uncaught exception fails the test; there is no timeout unless pytest-timeout is configured; fixtures from conftest.py run before the test; a Mock or MagicMock returns another mock unless given a return value."
        }
        "rspec" => {
            "RSpec; a failed expectation or an uncaught exception fails the example; there is no timeout; before hooks and let blocks run for each example; a double raises on messages it was not told to expect, and allow(...).to receive returns nil unless given a value."
        }
        "minitest" => {
            "Minitest; a failed assertion or an uncaught exception fails the test; there is no timeout; setup runs before each test; a stubbed method returns the value it was given."
        }
        "test-unit" => {
            "test-unit; a failed assertion or an uncaught exception fails the test; there is no timeout; setup runs before each test."
        }
        "cucumber" => {
            "Cucumber; a step that raises (a failed expectation included) fails the scenario; a step with no definition leaves it undefined, not passed; Before hooks run for each scenario."
        }
        "go-test" => {
            "go test; t.Error, t.Fatal and a panic fail the test; a test past the -timeout (default 10 minutes) fails; subtests run inside their parent; a nil pointer dereference panics. A test catches a changed value only where one of its checks compares it, directly or through what it feeds: a byte, field or result that no check reads can change while the test still passes."
        }
        "rust-libtest" | "rust-nextest" | "nextest" => {
            "Rust tests; a panic fails the test (a failed assert!, assert_eq!, unwrap or expect included) unless it is marked #[should_panic], which then fails when nothing panics; there is no timeout."
        }
        "rustdoc" => {
            "Rust doctests; the code block is compiled and run as its own program, and a panic fails it; a block marked no_run is only compiled and should_panic fails when nothing panics."
        }
        "junit-platform" => {
            "JUnit; a failed assertion or an uncaught exception fails the test (an expected one inside assertThrows aside); @BeforeEach runs before each test; there is no timeout unless one is set; a Mockito mock returns null, 0, false or an empty collection unless stubbed."
        }
        "testng" => {
            "TestNG; a failed assertion or an uncaught exception fails the test; @BeforeMethod runs before each test; there is no timeout unless one is set; a Mockito mock returns null, 0, false or an empty collection unless stubbed."
        }
        "unittest" => {
            "unittest; a failing assert* method or an uncaught exception fails the test; there is no timeout; setUp runs before each test; a Mock or MagicMock returns another mock unless given a return value."
        }
        _ => {
            "Vitest; a test past its timeout (default 5000 ms) fails; an unhandled error during the run fails it; vi.fn() returns undefined unless given an implementation."
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    /// `if`: the condition inverted, each direction asked separately.
    Invert,
    /// `return x`: returns undefined without evaluating `x`.
    ReturnUndefined,
    /// A declaration with a value: the value becomes undefined.
    ValueUndefined,
    /// An expression point that is not a statement (a JSX expression).
    ExpressionUndefined,
    /// Anything else: the statement is skipped.
    Skip,
}

/// The languages whose statements and tests this module can read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    JavaScript,
    Python,
    Ruby,
    Go,
    Rust,
    /// Java and Kotlin, told apart by file extension.
    Jvm,
}

impl Language {
    /// The run manifest's language name, when it is one of these.
    pub fn from_manifest(name: &str) -> Option<Self> {
        match name {
            "javascript" => Some(Self::JavaScript),
            "python" => Some(Self::Python),
            "ruby" => Some(Self::Ruby),
            "go" => Some(Self::Go),
            "rust" => Some(Self::Rust),
            "jvm" => Some(Self::Jvm),
            _ => None,
        }
    }
    fn comment(self, line: &str) -> bool {
        let t = line.trim_start();
        match self {
            Self::Python | Self::Ruby => t.starts_with('#'),
            Self::JavaScript | Self::Go | Self::Rust | Self::Jvm => {
                t.starts_with("//")
                    || t.starts_with("/*")
                    || t.starts_with("*/")
                    || t == "*"
                    || t.starts_with("* ")
                    || t.starts_with("*\t")
            }
        }
    }
    /// The value a removed value becomes, as the questions name it.
    fn nothing(self) -> &'static str {
        match self {
            Self::JavaScript => "undefined",
            Self::Python => "None",
            Self::Ruby => "nil",
            Self::Go => "the zero value",
            Self::Rust => "`Default::default()`",
            Self::Jvm => "the default value (null, or 0 or false for a primitive)",
        }
    }
}

/// What separates a test's name from the groups it sits in.
fn separator(language: Language, runner: &str) -> &'static str {
    match (language, runner) {
        (Language::JavaScript, _) => " > ",
        (Language::Python, _) => "::",
        (_, "rspec") => ":",
        (_, "minitest" | "test-unit" | "junit-platform" | "testng") => "#",
        (_, "go-test") => "/",
        (_, "rustdoc") => " - ",
        _ => "::",
    }
}

#[derive(Debug, Clone)]
pub struct Statement {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub change: Change,
    /// The value that replaces the statement's where the language chooses one
    /// by type (Rust), rather than a stand-in for nothing.
    pub replacement: Option<String>,
    /// Indexes into `Population::tests`: the passing tests that executed it.
    pub tests: Vec<usize>,
}

#[derive(Debug, Clone)]
pub struct TestRef {
    pub id: String,
    pub file: String,
    pub name: String,
    pub runner: String,
}

pub struct Population {
    pub root: PathBuf,
    pub language: Language,
    pub statements: Vec<Statement>,
    pub tests: Vec<TestRef>,
    /// Per test: the statement lines it ran, by file.
    ran: Vec<BTreeMap<String, BTreeSet<usize>>>,
    /// Files holding assessed statements; a test's own helpers are the files
    /// it imports that are not among these.
    source_files: BTreeSet<String>,
    /// The project's Java and Kotlin files, for JVM tests and their imports.
    jvm_files: Vec<String>,
    aliases: Vec<(String, String)>,
    /// Files read once: every request and every cache key reads the same text.
    files: std::sync::Mutex<BTreeMap<String, Option<std::sync::Arc<str>>>>,
}

/// The executed statements of a run and the tests that ran them. `sources` are
/// the run's source files, verified against the run.
pub fn population(
    root: &Path,
    report: &CoverageReport,
    sources: &BTreeMap<String, String>,
    language: Language,
) -> Result<Population, String> {
    let view = &report.filters.passed;
    let mut tests = Vec::new();
    let mut index = BTreeMap::new();
    // JVM tests are named by class; their file is found by its path.
    let jvm_files = if language == Language::Jvm {
        project_files(root, &["java", "kt"])
    } else {
        Vec::new()
    };
    for test in &view.tests {
        let file = match test.file.as_ref().filter(|f| !f.is_empty()) {
            Some(file) => file.clone(),
            None if language == Language::Jvm => {
                let found = jvm::class_paths(&test.name)
                    .iter()
                    .find_map(|suffix| jvm_files.iter().find(|f| ends_with_path(f, suffix)));
                match found {
                    Some(file) => file.clone(),
                    None => continue,
                }
            }
            None => continue,
        };
        if test.role != "test" {
            continue;
        }
        index.insert(test.id.as_str(), tests.len());
        tests.push(TestRef {
            id: test.id.clone(),
            file,
            name: test.name.clone(),
            runner: test.provenance.runner.clone(),
        });
    }
    let mut changes = BTreeMap::<String, BTreeMap<usize, Vec<(String, Option<Change>)>>>::new();
    let mut replacements = BTreeMap::<String, rust_source::Replacements>::new();
    let signatures = if language == Language::Rust {
        rust_source::signatures(
            sources
                .iter()
                .filter(|(file, _)| file.ends_with(".rs"))
                .map(|(_, source)| source.as_str()),
        )
    } else {
        rust_source::Signatures::default()
    };
    let mut statements = Vec::new();
    let mut seen = BTreeSet::new();
    // Python and Ruby record an `elif`/`elsif`, and Go, Rust and the JVM
    // languages may record an `else if`, as a decision only: its condition is
    // taken after every statement, where no statement starts on its line.
    let executed = view
        .points
        .iter()
        .filter(|p| p.measured && p.meta.kind == PointKind::Statement)
        .map(|p| (&p.meta.file, p.meta.line, &p.meta.source, &p.tests, false));
    let elifs = view
        .decisions
        .iter()
        .filter(|d| d.executed && language != Language::JavaScript)
        .map(|d| (&d.meta.file, d.meta.line, &d.meta.source, &d.tests, true));
    for (file, line, text, point_tests, decision) in executed.chain(elifs) {
        let key = (file.clone(), line);
        if !seen.insert(key) {
            continue; // the first statement on a line, as the audit reads it
        }
        let ids = point_tests
            .iter()
            .filter_map(|id| index.get(id.as_str()).copied())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            continue;
        }
        if language == Language::Rust && rust_source::test_path(file) {
            continue;
        }
        let Some(source) = sources.get(file) else {
            continue;
        };
        let lines = changes
            .entry(file.clone())
            .or_insert_with(|| match language {
                Language::JavaScript => statement_starts(file, source),
                Language::Python => python_statement_starts(source),
                Language::Ruby => ruby::statement_starts(source),
                Language::Go => go::statement_starts(source),
                Language::Rust => rust_source::statement_starts(source),
                Language::Jvm => jvm::statement_starts(file, source),
            });
        let Some(change) = change_at(lines, line, text, language) else {
            continue;
        };
        if decision && change != Change::Invert {
            continue;
        }
        let replacement = (language == Language::Rust
            && matches!(change, Change::ReturnUndefined | Change::ValueUndefined))
        .then(|| {
            let first = prefix(text.lines().next().unwrap_or("").trim(), 30);
            let first = first.trim_end_matches(';');
            replacements
                .entry(file.clone())
                .or_insert_with(|| rust_source::replacements(source, &signatures))
                .get(&line)
                .and_then(|all| all.iter().find(|(t, _)| t.starts_with(first)))
                .map(|(_, r)| r.clone())
        })
        .flatten();
        statements.push(Statement {
            file: file.clone(),
            line,
            text: text.clone(),
            change,
            replacement,
            tests: ids,
        });
    }
    let mut ran = vec![BTreeMap::<String, BTreeSet<usize>>::new(); tests.len()];
    for s in &statements {
        for &t in &s.tests {
            ran[t].entry(s.file.clone()).or_default().insert(s.line);
        }
    }
    let source_files = statements.iter().map(|s| s.file.clone()).collect();
    Ok(Population {
        root: root.to_path_buf(),
        language,
        statements,
        tests,
        ran,
        source_files,
        jvm_files,
        aliases: tsconfig_aliases(root),
        files: Default::default(),
    })
}

/// Every statement starting on each line, outermost first, with its text and
/// the change the audit applies to it (`None`: not assessed).
fn statement_starts(file: &str, source: &str) -> BTreeMap<usize, Vec<(String, Option<Change>)>> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(file).unwrap_or_else(|_| SourceType::tsx());
    let parsed = Parser::new(&allocator, source, source_type).parse();
    let starts = line_starts(source);
    struct Collector<'s> {
        source: &'s str,
        starts: &'s [usize],
        out: BTreeMap<usize, Vec<(String, Option<Change>)>>,
    }
    impl<'a> Visit<'a> for Collector<'_> {
        fn visit_statement(&mut self, statement: &JsStatement<'a>) {
            let span = statement.span();
            let line = line_of(self.starts, span.start as usize);
            let text = self
                .source
                .get(span.start as usize..span.end as usize)
                .unwrap_or("")
                .to_owned();
            self.out
                .entry(line)
                .or_default()
                .push((text, change_of(statement)));
            walk::walk_statement(self, statement);
        }
    }
    let mut collector = Collector {
        source,
        starts: &starts,
        out: BTreeMap::new(),
    };
    collector.visit_program(&parsed.program);
    collector.out
}

fn change_of(statement: &JsStatement) -> Option<Change> {
    Some(match statement {
        JsStatement::IfStatement(_) => Change::Invert,
        JsStatement::ReturnStatement(r) if r.argument.is_some() => Change::ReturnUndefined,
        JsStatement::VariableDeclaration(d) => {
            if d.declarations.iter().any(|d| d.init.is_some()) {
                Change::ValueUndefined
            } else {
                return None;
            }
        }
        JsStatement::ExportNamedDeclaration(e) => match &e.declaration {
            Some(Declaration::VariableDeclaration(d))
                if d.declarations.iter().any(|d| d.init.is_some()) =>
            {
                Change::ValueUndefined
            }
            _ => return None,
        },
        JsStatement::ExportDefaultDeclaration(e) => match &e.declaration {
            ExportDefaultDeclarationKind::FunctionDeclaration(_)
            | ExportDefaultDeclarationKind::ClassDeclaration(_)
            | ExportDefaultDeclarationKind::TSInterfaceDeclaration(_) => return None,
            _ => Change::Skip,
        },
        JsStatement::ImportDeclaration(_)
        | JsStatement::ExportAllDeclaration(_)
        | JsStatement::FunctionDeclaration(_)
        | JsStatement::ClassDeclaration(_)
        | JsStatement::TSTypeAliasDeclaration(_)
        | JsStatement::TSInterfaceDeclaration(_)
        | JsStatement::TSEnumDeclaration(_)
        | JsStatement::TSModuleDeclaration(_)
        | JsStatement::TSGlobalDeclaration(_)
        | JsStatement::TSImportEqualsDeclaration(_)
        | JsStatement::TSExportAssignment(_)
        | JsStatement::TSNamespaceExportDeclaration(_) => return None,
        _ => Change::Skip,
    })
}

/// The change for the measured statement on `line` whose text is `text`: the
/// first statement starting there with that text, or an expression point when
/// no statement starts there (a JSX expression is measured but not a statement).
fn change_at(
    starts: &BTreeMap<usize, Vec<(String, Option<Change>)>>,
    line: usize,
    text: &str,
    language: Language,
) -> Option<Change> {
    let first = prefix(text.lines().next().unwrap_or("").trim(), 30);
    let found = starts.get(&line).and_then(|all| {
        all.iter().find(|(t, _)| {
            let bare = t
                .strip_prefix("export ")
                .map(|t| t.strip_prefix("default ").unwrap_or(t))
                .unwrap_or(t);
            bare.starts_with(first) || t.starts_with(first)
        })
    });
    match (found, language) {
        (Some((_, change)), _) => *change,
        (None, Language::JavaScript) => Some(Change::ExpressionUndefined),
        // Elsewhere every statement is its own point; one no statement
        // matches is not assessed rather than guessed at.
        (None, _) => None,
    }
}

/// Every Python statement starting on each line, outermost first, with its
/// text and change: `if` inverted, `return x` returning None, an assignment's
/// value becoming None, anything else skipped; imports, definitions, `pass`,
/// `global`/`nonlocal` and bare strings (docstrings) are not assessed.
fn python_statement_starts(source: &str) -> BTreeMap<usize, Vec<(String, Option<Change>)>> {
    use ruff_python_ast::{
        Expr, Stmt,
        visitor::{Visitor, walk_stmt},
    };
    use ruff_text_size::Ranged;
    struct Collector<'s> {
        source: &'s str,
        starts: Vec<usize>,
        out: BTreeMap<usize, Vec<(String, Option<Change>)>>,
    }
    impl<'a> Visitor<'a> for Collector<'_> {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            let change = match stmt {
                Stmt::If(_) => Some(Change::Invert),
                Stmt::Return(r) => Some(if r.value.is_some() {
                    Change::ReturnUndefined
                } else {
                    Change::Skip
                }),
                Stmt::Assign(_) => Some(Change::ValueUndefined),
                Stmt::AnnAssign(a) => a.value.as_ref().map(|_| Change::ValueUndefined),
                Stmt::Import(_)
                | Stmt::ImportFrom(_)
                | Stmt::FunctionDef(_)
                | Stmt::ClassDef(_)
                | Stmt::Pass(_)
                | Stmt::Global(_)
                | Stmt::Nonlocal(_)
                | Stmt::TypeAlias(_)
                | Stmt::IpyEscapeCommand(_) => None,
                Stmt::Expr(e) if matches!(e.value.as_ref(), Expr::StringLiteral(_)) => None,
                _ => Some(Change::Skip),
            };
            let range = stmt.range();
            let (start, end) = (range.start().to_usize(), range.end().to_usize());
            let line = line_of(&self.starts, start);
            let text = self.source.get(start..end).unwrap_or("").to_owned();
            self.out.entry(line).or_default().push((text, change));
            // An `elif` is an `if` of its own, as JavaScript's `else if` is.
            if let Stmt::If(s) = stmt {
                // Its point is its condition, as coverage records it.
                for test in s.elif_else_clauses.iter().filter_map(|c| c.test.as_ref()) {
                    let start = test.range().start().to_usize();
                    let text = self.source.get(start..test.range().end().to_usize());
                    self.out
                        .entry(line_of(&self.starts, start))
                        .or_default()
                        .push((text.unwrap_or("").to_owned(), Some(Change::Invert)));
                }
            }
            walk_stmt(self, stmt);
        }
    }
    let Ok(parsed) = ruff_python_parser::parse_module(source) else {
        return BTreeMap::new();
    };
    let mut collector = Collector {
        source,
        starts: line_starts(source),
        out: BTreeMap::new(),
    };
    for stmt in &parsed.syntax().body {
        collector.visit_stmt(stmt);
    }
    collector.out
}

fn prefix(text: &str, chars: usize) -> &str {
    match text.char_indices().nth(chars) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

fn line_starts(source: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(source.match_indices('\n').map(|(i, _)| i + 1))
        .collect()
}

fn line_of(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|&s| s <= offset)
}

fn numbered(lines: &[&str]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("{:5} {l}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

fn stem(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .split('.')
        .next()
        .unwrap_or("")
        .to_lowercase()
}

fn describe(name: &str, separator: &str) -> String {
    let parts = name.split(separator).collect::<Vec<_>>();
    parts[..parts.len().saturating_sub(1)].join(separator)
}

impl Population {
    /// Read these files from here rather than the checkout: a run's sources
    /// as they were assessed, once the checkout has moved on.
    pub fn with_files(self, files: BTreeMap<String, String>) -> Self {
        {
            let mut cache = self
                .files
                .lock()
                .expect("the file cache is only read and filled");
            for (file, text) in files {
                cache.insert(file, Some(text.into()));
            }
        }
        self
    }

    /// Every file read so far, as it was read.
    pub fn files_read(&self) -> BTreeMap<String, String> {
        self.files
            .lock()
            .expect("the file cache is only read and filled")
            .iter()
            .filter_map(|(file, text)| text.as_ref().map(|t| (file.clone(), t.to_string())))
            .collect()
    }

    fn read(&self, file: &str) -> Option<std::sync::Arc<str>> {
        let mut files = self
            .files
            .lock()
            .expect("the file cache is only read and filled");
        files
            .entry(file.to_owned())
            .or_insert_with(|| {
                std::fs::read_to_string(self.root.join(file))
                    .ok()
                    .map(Into::into)
            })
            .clone()
    }

    /// The tests to ask about a statement, in order: tests in a file named like
    /// the source first, then one per describe block, round-robin over files.
    pub fn order(&self, statement: usize, k: usize) -> Vec<usize> {
        let s = &self.statements[statement];
        let base = stem(&s.file);
        let key = |&t: &usize| {
            let test = &self.tests[t];
            (
                !Path::new(&test.file)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&base),
                test.file.clone(),
                test.name.clone(),
            )
        };
        let mut all = s.tests.clone();
        all.sort_by_key(key);
        if all.len() <= k {
            return all;
        }
        let mut groups = Vec::<((String, String), Vec<usize>)>::new();
        for &t in &all {
            let g = (
                self.tests[t].file.clone(),
                describe(
                    &self.tests[t].name,
                    separator(self.language, &self.tests[t].runner),
                ),
            );
            match groups.iter_mut().find(|(k, _)| *k == g) {
                Some((_, v)) => v.push(t),
                None => groups.push((g, vec![t])),
            }
        }
        let mut by_file = Vec::<(String, Vec<Vec<usize>>)>::new();
        for ((file, _), members) in groups {
            match by_file.iter_mut().find(|(f, _)| *f == file) {
                Some((_, v)) => v.push(members),
                None => by_file.push((file, vec![members])),
            }
        }
        by_file.sort_by_key(|(f, _)| {
            (
                !Path::new(f)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&base),
                f.clone(),
            )
        });
        let mut picked = Vec::new();
        let mut depth = 0;
        while picked.len() < k && by_file.iter().any(|(_, g)| g.len() > depth) {
            for (_, g) in &by_file {
                if depth < g.len() && picked.len() < k {
                    picked.push(g[depth][0]);
                }
            }
            depth += 1;
        }
        for t in all {
            if picked.len() >= k {
                break;
            }
            if !picked.contains(&t) {
                picked.push(t);
            }
        }
        picked
    }

    /// The test's code: the file's first lines and the test's own body, found
    /// by its title, comment lines left out.
    pub fn test_code(&self, test: usize) -> String {
        match self.language {
            Language::Python => return self.python_test_code(test),
            Language::JavaScript => {}
            _ => return self.located_test_code(test),
        }
        let t = &self.tests[test];
        let text = self.read(&t.file).unwrap_or_default();
        let lines = text.split('\n').collect::<Vec<_>>();
        let title = t.name.rsplit(" > ").next().unwrap_or(&t.name);
        let needle = prefix(title, 40);
        let start = (!needle.is_empty())
            .then(|| lines.iter().position(|l| l.contains(needle)))
            .flatten();
        let fmt = |i: usize| format!("{:5} {}", i + 1, lines[i]);
        let head = (0..lines.len().min(HEAD_LINES))
            .filter(|&i| !self.language.comment(lines[i]))
            .map(fmt)
            .collect::<Vec<_>>();
        let Some(start) = start else {
            let rest = (30.min(lines.len())..230.min(lines.len())).map(fmt);
            return head
                .into_iter()
                .chain(std::iter::once("  ...".to_owned()))
                .chain(rest)
                .collect::<Vec<_>>()
                .join("\n");
        };
        let indent = lines[start].len() - lines[start].trim_start().len();
        let mut body = Vec::new();
        for (i, l) in lines
            .iter()
            .copied()
            .enumerate()
            .take(start + BODY_LINES)
            .skip(start)
        {
            if i > start && is_test_call(l) && l.len() - l.trim_start().len() <= indent {
                break;
            }
            if !self.language.comment(l) {
                body.push(fmt(i));
            }
        }
        let mut out = head;
        if start > HEAD_LINES {
            out.push("  ...".into());
        }
        out.extend(body);
        out.join("\n")
    }

    /// A Ruby, Go, Rust or JVM test's code: the file's first lines, the lines
    /// that frame it (its class or groups, its annotations), and its body.
    fn located_test_code(&self, test: usize) -> String {
        let t = &self.tests[test];
        let text = self.read(&t.file).unwrap_or_default();
        let lines = text.split('\n').collect::<Vec<_>>();
        let located = match self.language {
            Language::Ruby => ruby::locate(&text, &lines, &t.runner, &t.name),
            Language::Go => go::locate(&lines, &t.name),
            Language::Rust => rust_source::locate(&lines, &t.name),
            _ => jvm::locate(&lines, &t.name),
        };
        let fmt = |i: usize| format!("{:5} {}", i + 1, lines[i]);
        // Lines inside `/* ... */` (a license header, say) are comments too.
        let mut open = false;
        let commented = lines
            .iter()
            .map(|l| {
                let t = l.trim();
                let was = open;
                if self.language != Language::Ruby {
                    if !open && t.starts_with("/*") {
                        open = true;
                    }
                    if open && t.contains("*/") {
                        open = false;
                        return true;
                    }
                }
                was || open || self.language.comment(l)
            })
            .collect::<Vec<_>>();
        // The head: the file's first code lines (its package and imports).
        let head = (0..lines.len())
            .filter(|&i| !commented[i])
            .take(HEAD_LINES)
            .collect::<Vec<_>>();
        let head_end = head.last().map_or(0, |&i| i + 1);
        let mut out = head.iter().map(|&i| fmt(i)).collect::<Vec<_>>();
        let Some(located) = located else {
            out.push("  ...".into());
            out.extend((head_end..(head_end + 200).min(lines.len())).map(fmt));
            return out.join("\n");
        };
        let end = located
            .end
            .min(located.start + BODY_LINES)
            .min(lines.len().saturating_sub(1));
        let mut last = head_end;
        let mut shown = located.context.iter().copied().collect::<BTreeSet<_>>();
        shown.extend(located.start..=end);
        // What the test uses from its own file: a table it loops over, a
        // helper it calls, a `let` it reads.
        shown.extend(used_definitions(&lines, self.language, located.start, end));
        for i in shown {
            // A doctest lives in comments: its lines are the test.
            if i < last || (commented[i] && self.language != Language::Rust) {
                continue;
            }
            if i > last {
                out.push("  ...".into());
            }
            out.push(fmt(i));
            last = i + 1;
        }
        out.join("\n")
    }

    /// A Python test's code: the file's first lines, the class holding the
    /// test, its decorators, and its body, found by the test function's name
    /// and ended by indentation.
    fn python_test_code(&self, test: usize) -> String {
        let t = &self.tests[test];
        let text = self.read(&t.file).unwrap_or_default();
        let lines = text.split('\n').collect::<Vec<_>>();
        let title = python_title(&t.name);
        let indent_of = |l: &str| l.len() - l.trim_start().len();
        let start = (!title.is_empty())
            .then(|| {
                lines.iter().position(|l| {
                    let t = l.trim_start();
                    t.starts_with(&format!("def {title}("))
                        || t.starts_with(&format!("async def {title}("))
                })
            })
            .flatten();
        let fmt = |i: usize| format!("{:5} {}", i + 1, lines[i]);
        let mut out = (0..lines.len().min(HEAD_LINES))
            .filter(|&i| !self.language.comment(lines[i]))
            .map(fmt)
            .collect::<Vec<_>>();
        let Some(start) = start else {
            out.push("  ...".into());
            out.extend((HEAD_LINES.min(lines.len())..(HEAD_LINES + 200).min(lines.len())).map(fmt));
            return out.join("\n");
        };
        let indent = indent_of(lines[start]);
        let mut first = start;
        while first > 0
            && lines[first - 1].trim_start().starts_with('@')
            && indent_of(lines[first - 1]) == indent
        {
            first -= 1;
        }
        let class = (indent > 0)
            .then(|| {
                (0..first).rev().find(|&i| {
                    lines[i].trim_start().starts_with("class ") && indent_of(lines[i]) < indent
                })
            })
            .flatten();
        for i in class.into_iter().chain(first..start) {
            if i >= HEAD_LINES {
                if out.last().is_some_and(|l| l != "  ...") {
                    out.push("  ...".into());
                }
                out.push(fmt(i));
            }
        }
        if start >= HEAD_LINES
            && out.last().is_some_and(|l| l != "  ...")
            && first == start
            && class.is_none()
        {
            out.push("  ...".into());
        }
        for (i, &l) in lines.iter().enumerate().skip(start).take(BODY_LINES) {
            if i > start && !l.trim().is_empty() && indent_of(l) <= indent {
                break;
            }
            if !self.language.comment(l) && i >= HEAD_LINES {
                out.push(fmt(i));
            }
        }
        out.join("\n")
    }

    /// Test-side files the test imports (helpers, fixtures), cut to a budget
    /// that grows with the number of questions.
    pub fn helpers(&self, test: usize, questions: usize) -> BTreeMap<String, String> {
        let cap = HELPER_CHARS.min(1500 + 400 * questions);
        let t = &self.tests[test];
        let text = self.read(&t.file).unwrap_or_default();
        let mut out = BTreeMap::new();
        let mut size = 0;
        let files = match self.language {
            Language::JavaScript => import_specifiers(&text)
                .iter()
                .filter_map(|spec| self.resolve(&t.file, spec))
                .collect::<Vec<_>>(),
            Language::Python => self.python_helpers(&t.file, &text),
            Language::Ruby => ruby::helpers(&self.root, &t.file, &text),
            Language::Go => go::helpers(&self.root, &t.file, &text),
            Language::Rust => rust_source::helpers(&self.root, &t.file, &text),
            Language::Jvm => jvm::imports(&text)
                .iter()
                .filter_map(|import| {
                    jvm::class_paths(&import.replace('/', "."))
                        .iter()
                        .find_map(|suffix| {
                            self.jvm_files.iter().find(|f| ends_with_path(f, suffix))
                        })
                        .cloned()
                })
                .collect(),
        };
        for file in files {
            if out.contains_key(&file) {
                continue;
            }
            if self.source_files.contains(&file) || file.contains("node_modules") {
                continue;
            }
            let mut body = self.read(&file).map(|b| b.to_string()).unwrap_or_default();
            if size + body.len() > cap {
                let keep = cap.saturating_sub(size);
                let cut = body
                    .char_indices()
                    .map(|(i, _)| i)
                    .take_while(|&i| i <= keep)
                    .last()
                    .unwrap_or(0);
                body = format!("{}\n... (cut)", &body[..cut]);
            }
            size += body.len();
            out.insert(file, numbered(&body.split('\n').collect::<Vec<_>>()));
            if size >= cap {
                break;
            }
        }
        if self.language == Language::Ruby {
            let mut used = 0;
            for file in ruby::support_files(&self.root, &text) {
                if out.contains_key(&file) || used >= FIXTURE_CHARS {
                    continue;
                }
                let mut body = self.read(&file).map(|b| b.to_string()).unwrap_or_default();
                if used + body.len() > FIXTURE_CHARS {
                    let keep = FIXTURE_CHARS - used;
                    let cut = body
                        .char_indices()
                        .map(|(i, _)| i)
                        .take_while(|&i| i <= keep)
                        .last()
                        .unwrap_or(0);
                    body = format!("{}\n... (cut)", &body[..cut]);
                }
                used += body.len();
                out.insert(file, numbered(&body.split('\n').collect::<Vec<_>>()));
            }
        }
        out
    }

    /// A Python test's helpers: the `conftest.py` files from its directory up
    /// (where pytest fixtures live, closest first), then the project modules it
    /// imports.
    fn python_helpers(&self, from: &str, text: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut dir = Path::new(from).parent();
        while let Some(d) = dir {
            let conftest = d.join("conftest.py").to_string_lossy().replace('\\', "/");
            if self.root.join(&conftest).is_file() {
                out.push(conftest);
            }
            dir = d.parent();
        }
        let here = Path::new(from).parent().unwrap_or(Path::new(""));
        for module in python_imports(text) {
            let dots = module.chars().take_while(|c| *c == '.').count();
            let rest = module[dots..].replace('.', "/");
            let bases = if dots > 0 {
                let mut base = here.to_path_buf();
                for _ in 1..dots {
                    base.pop();
                }
                vec![base]
            } else {
                vec![PathBuf::new(), here.to_path_buf(), PathBuf::from("src")]
            };
            for base in bases {
                let path = base.join(&rest);
                let path = path.to_string_lossy().replace('\\', "/");
                let found = [format!("{path}.py"), format!("{path}/__init__.py")]
                    .into_iter()
                    .find(|c| !c.starts_with('/') && self.root.join(c).is_file());
                if let Some(found) = found {
                    out.push(found);
                    break;
                }
            }
        }
        out
    }

    fn resolve(&self, from: &str, spec: &str) -> Option<String> {
        let base = if let Some((alias, target)) = self
            .aliases
            .iter()
            .find(|(a, _)| spec.starts_with(a.as_str()))
        {
            PathBuf::from(format!("{target}{}", &spec[alias.len()..]))
        } else if spec.starts_with('.') {
            Path::new(from).parent().unwrap_or(Path::new("")).join(spec)
        } else {
            return None;
        };
        let p = normalize(&base);
        let s = p.to_string_lossy().to_string();
        let candidates = [
            s.clone(),
            format!("{s}.ts"),
            format!("{s}.tsx"),
            format!("{s}.js"),
            format!("{s}.mjs"),
            s.replace(".js", ".ts"),
            format!("{s}/index.ts"),
            format!("{s}/index.tsx"),
            format!("{s}/index.js"),
        ];
        candidates.into_iter().find(|c| self.root.join(c).is_file())
    }

    /// The source this test ran: every statement it executed marked ▶ with two
    /// code lines around it, eight around each asked statement, comment lines
    /// left out, within a budget that grows with the number of questions.
    pub fn code_run(&self, test: usize, asked: &[usize]) -> BTreeMap<String, String> {
        let budget = CODE_CHARS.min(3000 + 1500 * asked.len());
        let ran = &self.ran[test];
        let mut want = BTreeMap::<String, BTreeSet<usize>>::new();
        for &a in asked {
            want.entry(self.statements[a].file.clone())
                .or_default()
                .insert(self.statements[a].line);
        }
        let mut files = want.keys().cloned().collect::<Vec<_>>();
        let mut others = ran
            .iter()
            .filter(|(f, _)| !want.contains_key(*f))
            .collect::<Vec<_>>();
        others.sort_by_key(|(f, lines)| (std::cmp::Reverse(lines.len()), (*f).clone()));
        files.extend(others.into_iter().map(|(f, _)| f.clone()));
        let empty = BTreeSet::new();
        let mut out = BTreeMap::new();
        let mut total = 0;
        for f in files {
            let Some(source) = self.read(&f) else {
                continue;
            };
            let text = source.split('\n').collect::<Vec<_>>();
            let r = ran.get(&f).unwrap_or(&empty);
            let code = (1..=text.len())
                .filter(|&i| !self.language.comment(text[i - 1]))
                .collect::<Vec<_>>();
            let pos = code
                .iter()
                .enumerate()
                .map(|(n, &i)| (i, n))
                .collect::<BTreeMap<_, _>>();
            let around = |l: usize, w: usize| -> Vec<usize> {
                match pos.get(&l) {
                    Some(&n) => code[n.saturating_sub(w)..(n + w + 1).min(code.len())].to_vec(),
                    None => (l.saturating_sub(w).max(1)..=(l + w).min(text.len())).collect(),
                }
            };
            let mut show = BTreeSet::new();
            for &l in r {
                show.extend(around(l, CONTEXT_LINES));
            }
            for &l in want.get(&f).unwrap_or(&empty) {
                show.extend(around(l, WINDOW_LINES));
            }
            let mut lines = Vec::new();
            let mut last = 0;
            for i in show {
                if i == 0 || i > text.len() || self.language.comment(text[i - 1]) {
                    continue;
                }
                if i != last + 1 {
                    lines.push(format!("        ... {} lines not shown", i - last - 1));
                }
                lines.push(format!(
                    "{}{:5} {}",
                    if r.contains(&i) { '▶' } else { ' ' },
                    i,
                    text[i - 1]
                ));
                last = i;
            }
            let body = lines.join("\n");
            if !want.contains_key(&f) && total + body.len() > budget {
                continue;
            }
            total += body.len();
            out.insert(f, body);
        }
        out
    }

    /// The questions for one statement: one per change (two for an `if`).
    pub fn questions(&self, statement: usize) -> Vec<String> {
        let s = &self.statements[statement];
        let line = s.text.lines().next().unwrap_or("").trim();
        let changes = match (s.change, &s.replacement) {
            (Change::ReturnUndefined | Change::ValueUndefined, Some(r))
                if self.language == Language::Rust =>
            {
                vec![rust_question(line, s.change, r)]
            }
            _ => self.default_questions(s, line),
        };
        changes
            .into_iter()
            .map(|c| format!("{}:{}: {c}.", s.file, s.line))
            .collect()
    }

    fn default_questions(&self, s: &Statement, line: &str) -> Vec<String> {
        match s.change {
            // Go and the JVM languages replace a value after it is computed,
            // as their mutation testers do: the expression still runs.
            Change::ReturnUndefined if self.language == Language::Go => vec![format!(
                "`{line}` returns the zero value of each result instead: nil for an error, pointer, slice, map or interface, 0, \"\" or false, or an empty struct (its expressions still run)"
            )],
            Change::ReturnUndefined if self.language == Language::Jvm => vec![format!(
                "`{line}` returns {} instead (its expression still runs)",
                self.language.nothing()
            )],
            Change::ReturnUndefined if matches!(self.language, Language::Ruby | Language::Rust) => {
                let nothing = match self.language {
                    Language::Rust
                        if ["Ok(", "Err(", "return Ok(", "return Err("]
                            .iter()
                            .any(|p| line.starts_with(p)) =>
                    {
                        "`Ok(Default::default())`"
                    }
                    other => other.nothing(),
                };
                vec![if line.starts_with("return") {
                    format!("`{line}` returns {nothing} instead (the expression is not evaluated)")
                } else {
                    format!(
                        "`{line}`, the value the enclosing block or method produces, becomes {nothing} (it is not evaluated)"
                    )
                }]
            }
            Change::ValueUndefined if self.language == Language::Go => vec![format!(
                "`{line}`: after it runs, the variables it assigns hold the zero value of their type instead"
            )],
            Change::ValueUndefined if self.language == Language::Jvm => vec![format!(
                "`{line}`: after it runs, the variable it assigns holds {} instead",
                self.language.nothing()
            )],
            Change::ValueUndefined if matches!(self.language, Language::Ruby | Language::Rust) => {
                let nothing = self.language.nothing();
                vec![match split_initializer(line) {
                    Some(head) => format!(
                        "`{line}`: the value after `{head}` becomes {nothing} (it is not evaluated)"
                    ),
                    None => format!("`{line}`: the assigned value becomes {nothing}"),
                }]
            }
            Change::ReturnUndefined if self.language == Language::Python => vec![format!(
                "`{line}` becomes `return None` (the expression is not evaluated)"
            )],
            Change::ReturnUndefined => vec![format!(
                "`{line}` becomes `return undefined;` (the expression is not evaluated)"
            )],
            Change::ValueUndefined if self.language == Language::Python => {
                vec![match split_initializer(line) {
                    Some(head) => {
                        format!("`{line}` becomes `{head} None` (the value is not evaluated)")
                    }
                    None => format!("`{line}`: the assigned value becomes None"),
                }]
            }
            Change::ValueUndefined => vec![match split_initializer(line) {
                Some(head) => format!(
                    "`{line}` becomes `{head} undefined` (the initializer is not evaluated)"
                ),
                None => format!("`{line}`: the declared value becomes undefined"),
            }],
            Change::Skip => vec![format!("`{line}` is deleted (it never runs)")],
            Change::ExpressionUndefined => vec![format!(
                "the expression `{line}` evaluates to undefined instead (it is not evaluated)"
            )],
            Change::Invert => {
                let c = match self.language {
                    Language::Python => python_condition(line),
                    Language::JavaScript => condition(line),
                    Language::Ruby => ruby::condition(line),
                    Language::Go => go::condition(line),
                    Language::Rust => rust_source::condition(line),
                    Language::Jvm => jvm::condition(line),
                };
                // A pattern match (`if let`) can be made to fail, not to
                // succeed: its bindings would have no value.
                if c.starts_with("let ") {
                    return vec![format!(
                        "`{line}`: whenever the pattern `{c}` matches it is treated as not matching (the branch it would enter is skipped)"
                    )];
                }
                vec![
                    format!(
                        "`{line}`: whenever the condition `{c}` is true it is treated as false (the branch it would enter is skipped)"
                    ),
                    format!(
                        "`{line}`: whenever the condition `{c}` is false it is treated as true (the branch is entered)"
                    ),
                ]
            }
        }
    }

    /// The request for one test and the statements asked about it.
    pub fn request(&self, test: usize, asked: &[usize], model: &str) -> Value {
        let t = &self.tests[test];
        let mut questions = serde_json::Map::new();
        for (n, &a) in asked.iter().enumerate() {
            for (j, q) in self.questions(a).into_iter().enumerate() {
                questions.insert(
                    format!("q{n}_{j}"),
                    json!({"type": "noul", "instructions": {"task": q}, "criteria": {"true": "The test fails.", "false": "The test still passes."}}),
                );
            }
        }
        json!({"model": model, "state": {
            "instructions": INSTRUCTIONS, "runtime": runtime(&t.runner),
            "test": {"file": t.file, "name": t.name},
            "test_code": self.test_code(test), "helpers": self.helpers(test, asked.len()),
            "code_run": self.code_run(test, asked)}, "questions": questions})
    }

    /// What Jev sees for one statement asked of one test, line numbers aside:
    /// the same code renumbered is the same question. Equal keys, equal question.
    /// `salt` carries what changes every answer at once: the run's dependency
    /// and configuration fingerprints.
    pub fn pair_key(&self, statement: usize, test: usize, model: &str, salt: &str) -> String {
        let t = &self.tests[test];
        let parts = json!([
            model,
            salt,
            INSTRUCTIONS,
            runtime(&t.runner),
            true,
            &t.file,
            &t.name,
            strip_numbers(&self.test_code(test)),
            self.helpers(test, 1)
                .into_iter()
                .map(|(f, b)| (f, strip_numbers(&b)))
                .collect::<BTreeMap<_, _>>(),
            self.code_run(test, &[statement])
                .into_iter()
                .map(|(f, b)| (f, strip_numbers(&b)))
                .collect::<BTreeMap<_, _>>(),
            self.questions(statement)
                .iter()
                .map(|q| strip_location(q))
                .collect::<Vec<_>>(),
        ]);
        format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&parts).expect("serializable key"))
        )
    }
}

/// A Rust return or assignment's change, as the line it becomes.
fn rust_question(line: &str, change: Change, replacement: &str) -> String {
    let repl = replacement.split_whitespace().collect::<Vec<_>>().join(" ");
    match change {
        Change::ReturnUndefined if line.starts_with("return") => {
            format!("`{line}` becomes `return {repl};`")
        }
        Change::ReturnUndefined => {
            format!("`{line}` becomes `{repl}` (it is the value the enclosing block produces)")
        }
        _ => match split_initializer(line) {
            Some(head) => format!("`{line}` becomes `{head} {repl};`"),
            None => format!("`{line}`: the value it assigns becomes `{repl}`"),
        },
    }
}

/// Write files' text, compressed, to `path`.
pub fn save_snapshot(path: &Path, files: &BTreeMap<String, String>) -> Result<(), String> {
    use std::io::Write;
    let bytes = serde_json::to_vec(files).map_err(|e| e.to_string())?;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&bytes).map_err(|e| e.to_string())?;
    let compressed = encoder.finish().map_err(|e| e.to_string())?;
    let temp = path.with_extension("tmp");
    std::fs::write(&temp, compressed).map_err(|e| e.to_string())?;
    std::fs::rename(&temp, path).map_err(|e| e.to_string())
}

/// Read files written by [`save_snapshot`].
pub fn load_snapshot(path: &Path) -> Option<BTreeMap<String, String>> {
    use std::io::Read;
    let compressed = std::fs::read(path).ok()?;
    let mut text = Vec::new();
    flate2::read::GzDecoder::new(compressed.as_slice())
        .read_to_end(&mut text)
        .ok()?;
    serde_json::from_slice(&text).ok()
}

/// Where a test sits in its file: its first line, its last, and lines that
/// frame it (its class, groups or annotations), all zero-based.
struct Located {
    context: Vec<usize>,
    start: usize,
    end: usize,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start().len()
}

/// The line closing the braces opened from `start` on (strings, characters
/// and line comments aside).
fn brace_end(lines: &[&str], start: usize) -> usize {
    let mut depth = 0i64;
    let mut opened = false;
    for (i, line) in lines.iter().enumerate().skip(start).take(BODY_LINES) {
        let mut chars = line.chars().peekable();
        let mut quote: Option<char> = None;
        while let Some(c) = chars.next() {
            match (quote, c) {
                (Some(_), '\\') => {
                    chars.next();
                }
                (Some(q), c) if c == q => quote = None,
                (Some(_), _) => {}
                (None, '"' | '`') => quote = Some(c),
                (None, '/') if chars.peek() == Some(&'/') => break,
                (None, '{') => {
                    depth += 1;
                    opened = true;
                }
                (None, '}') => depth -= 1,
                _ => {}
            }
        }
        if opened && depth <= 0 {
            return i;
        }
    }
    (start + BODY_LINES).min(lines.len()).saturating_sub(1)
}

/// The `end` (or `}`) closing a Ruby block or method opened on `start`.
fn keyword_end(lines: &[&str], start: usize) -> usize {
    let first = lines[start].trim_end();
    if first.ends_with(" end") || first.ends_with('}') {
        return start;
    }
    let depth = indent(lines[start]);
    (start + 1..lines.len().min(start + BODY_LINES))
        .find(|&i| {
            let t = lines[i].trim_start();
            indent(lines[i]) == depth && (t == "end" || t.starts_with("end ") || t.starts_with('}'))
        })
        .unwrap_or((start + BODY_LINES).min(lines.len()).saturating_sub(1))
}

const DEFINITION_LINES: usize = 120;

/// The lines of the test file's own definitions that the test at
/// `start..=end` names, and those they name in turn, up to a budget.
fn used_definitions(lines: &[&str], language: Language, start: usize, end: usize) -> Vec<usize> {
    let words = |range: std::ops::RangeInclusive<usize>| {
        let mut out = BTreeSet::new();
        for i in range {
            let line = lines.get(i).copied().unwrap_or("");
            out.extend(
                line.split(|c: char| !(c.is_alphanumeric() || c == '_'))
                    .filter(|w| !w.is_empty())
                    .map(str::to_owned),
            );
        }
        out
    };
    let definitions = definitions(lines, language);
    let mut wanted = words(start..=end);
    let mut picked = BTreeSet::new();
    let mut budget = DEFINITION_LINES;
    for _ in 0..2 {
        let mut next = BTreeSet::new();
        for (name, a, b) in &definitions {
            let (a, b) = (*a, *b);
            if picked.contains(&a) || (a >= start && a <= end) || !wanted.contains(name) {
                continue;
            }
            let size = b - a + 1;
            if size > budget {
                continue;
            }
            budget -= size;
            picked.insert(a);
            next.extend(words(a..=b));
            picked.extend(a..=b);
        }
        wanted = next;
    }
    picked.into_iter().collect()
}

/// A file's named definitions (functions, methods, table variables, RSpec
/// `let`/`subject`) with their first and last lines.
fn definitions(lines: &[&str], language: Language) -> Vec<(String, usize, usize)> {
    let ident = |s: &str| {
        s.chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '?' || *c == '!')
            .collect::<String>()
    };
    let mut out = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim_start();
        let name = match language {
            Language::Go => t
                .strip_prefix("func ")
                .map(|r| match r.strip_prefix('(') {
                    Some(r) => r.split_once(')').map_or("", |(_, r)| r.trim_start()),
                    None => r,
                })
                .or_else(|| t.strip_prefix("var "))
                .or_else(|| t.strip_prefix("type "))
                .filter(|_| indent(line) == 0)
                .map(ident),
            Language::Rust => {
                let r = t.strip_prefix("pub ").unwrap_or(t);
                let r = r.strip_prefix("pub(crate) ").unwrap_or(r);
                ["fn ", "const ", "static ", "struct ", "enum "]
                    .iter()
                    .find_map(|k| r.strip_prefix(k))
                    .map(ident)
            }
            Language::Ruby => t
                .strip_prefix("def ")
                .map(|r| ident(r.strip_prefix("self.").unwrap_or(r)))
                .or_else(|| {
                    ["let(:", "let!(:", "subject(:"]
                        .iter()
                        .find_map(|k| t.strip_prefix(k))
                        .map(ident)
                })
                .or_else(|| {
                    (t.starts_with("subject ") || t.starts_with("subject{"))
                        .then(|| "subject".into())
                }),
            Language::Jvm => {
                if let Some(r) = t.split_once("fun ").filter(|(head, _)| !head.contains('(')) {
                    Some(ident(r.1.trim_start_matches('`')))
                } else if t.contains('(')
                    && !t.ends_with(';')
                    && !t.starts_with(['@', '}', '.', '/', '*', '+', '"'])
                    && ![
                        "return", "new ", "if", "for", "while", "switch", "catch", "else", "try",
                        "throw", "assert", "super", "this",
                    ]
                    .iter()
                    .any(|k| t.starts_with(k))
                {
                    let head = t.split('(').next().unwrap_or("");
                    let mut parts = head.split_whitespace().rev();
                    let name = parts.next().unwrap_or("");
                    (parts.next().is_some() && !head.contains('=')).then(|| ident(name))
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(name) = name.filter(|n| !n.is_empty()) else {
            continue;
        };
        let end = match language {
            Language::Ruby => keyword_end(lines, i),
            _ if line.contains('{')
                || lines
                    .get(i + 1)
                    .is_some_and(|n| n.trim_start().starts_with('{')) =>
            {
                brace_end(lines, i)
            }
            _ => i,
        };
        out.push((name, i, end));
    }
    out
}

/// Whether `path` is `suffix` or ends with `/suffix`.
fn ends_with_path(path: &str, suffix: &str) -> bool {
    path.strip_suffix(suffix)
        .is_some_and(|head| head.is_empty() || head.ends_with('/'))
}

/// The project's files with these extensions, as project-relative paths
/// (ignored files, such as build output, left out).
fn project_files(root: &Path, extensions: &[&str]) -> Vec<String> {
    let mut out = ignore::WalkBuilder::new(root)
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| extensions.contains(&x))
        })
        .filter_map(|e| {
            e.path()
                .strip_prefix(root)
                .ok()
                .map(|p| p.to_string_lossy().replace('\\', "/"))
        })
        .filter(|p| !p.starts_with(".supercov/"))
        .collect::<Vec<_>>();
    out.sort();
    out
}

fn is_test_call(line: &str) -> bool {
    let t = line.trim_start();
    let rest = if let Some(r) = t.strip_prefix("it") {
        r
    } else if let Some(r) = t.strip_prefix("test") {
        r
    } else {
        return false;
    };
    let rest = if let Some(r) = rest.strip_prefix('.') {
        r.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_')
    } else {
        rest
    };
    rest.trim_start().starts_with('(')
}

/// `const x = value` -> `const x =`; `None` when there is no plain `=`.
fn split_initializer(line: &str) -> Option<String> {
    let b = line.as_bytes();
    for i in 1..b.len() {
        if b[i] == b'='
            && !matches!(b[i - 1], b'=' | b'!' | b'<' | b'>')
            && !matches!(b.get(i + 1), Some(b'=') | Some(b'>'))
        {
            return Some(line[..=i].to_owned());
        }
    }
    None
}

/// `if c:` -> `c`
/// A Python test's function name: pytest `file::Class::test[param]`,
/// unittest `module.Class.test`.
fn python_title(name: &str) -> &str {
    let last = name.rsplit("::").next().unwrap_or(name);
    let last = last.split('[').next().unwrap_or(last);
    last.rsplit('.').next().unwrap_or(last)
}

fn python_condition(line: &str) -> String {
    let t = line.trim();
    let t = t
        .strip_prefix("elif")
        .or_else(|| t.strip_prefix("if"))
        .map(str::trim_start)
        .unwrap_or(t);
    t.strip_suffix(':')
        .map(str::trim_end)
        .unwrap_or(t)
        .to_owned()
}

/// The modules a Python file imports: `from a.b import c` -> `a.b`,
/// `import a, b as x` -> `a`, `b`, `from . import c` -> `.c`.
fn python_imports(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix("from ") {
            let Some((module, names)) = rest.split_once(" import ") else {
                continue;
            };
            let module = module.trim();
            if module.chars().all(|c| c == '.') {
                for name in names.trim_matches(|c| c == '(' || c == ')').split(',') {
                    let name = name.split(" as ").next().unwrap_or("").trim();
                    if !name.is_empty() && name != "*" {
                        out.push(format!("{module}{name}"));
                    }
                }
            } else {
                out.push(module.to_owned());
            }
        } else if let Some(rest) = t.strip_prefix("import ") {
            for part in rest.split(',') {
                let module = part.split(" as ").next().unwrap_or("").trim();
                if !module.is_empty() {
                    out.push(module.to_owned());
                }
            }
        }
    }
    out
}

/// `if (c) {` -> `c`
fn condition(line: &str) -> String {
    let mut t = line.trim();
    if let Some(r) = t.strip_prefix('}') {
        let r = r.trim_start();
        if let Some(r) = r.strip_prefix("else") {
            t = r.trim_start();
        }
    }
    let Some(r) = t.strip_prefix("if") else {
        return line.to_owned();
    };
    let r = r.trim_start();
    let Some(r) = r.strip_prefix('(') else {
        return line.to_owned();
    };
    let r = r.trim_end();
    let r = r.strip_suffix('{').map(str::trim_end).unwrap_or(r);
    match r.strip_suffix(')') {
        Some(c) => c.to_owned(),
        None => line.to_owned(),
    }
}

fn strip_numbers(text: &str) -> String {
    text.split('\n')
        .map(|l| {
            let (mark, rest) = match l.chars().next() {
                Some(c @ ('▶' | ' ')) => (Some(c), &l[c.len_utf8()..]),
                _ => (None, l),
            };
            let trimmed = rest.trim_start();
            let digits = trimmed.len()
                - trimmed
                    .trim_start_matches(|c: char| c.is_ascii_digit())
                    .len();
            if digits > 0 && trimmed[digits..].starts_with(' ') {
                format!(
                    "{}{}",
                    mark.map(String::from).unwrap_or_default(),
                    &trimmed[digits + 1..]
                )
            } else if let Some(n) = l
                .trim_start()
                .strip_prefix("... ")
                .and_then(|r| r.split_once(" lines not shown"))
            {
                let _ = n;
                "...".to_owned()
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn strip_location(question: &str) -> String {
    // `file:12: change` -> `file:: change`
    match question.split_once(": ") {
        Some((loc, rest)) => match loc.rsplit_once(':') {
            Some((file, line)) if line.chars().all(|c| c.is_ascii_digit()) => {
                format!("{file}:: {rest}")
            }
            _ => question.to_owned(),
        },
        None => question.to_owned(),
    }
}

fn import_specifiers(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.split('\n') {
        let t = line.trim_start();
        let quoted = |s: &str| -> Option<String> {
            let s = s.trim_start();
            let q = s.chars().next().filter(|c| *c == '\'' || *c == '"')?;
            let rest = &s[1..];
            rest.find(q).map(|end| rest[..end].to_owned())
        };
        if (t.starts_with("import ") || t.starts_with("export "))
            && let Some(i) = t.find(" from ")
        {
            out.extend(quoted(&t[i + 6..]));
        } else if let Some(i) = t.find("import(") {
            out.extend(quoted(&t[i + 7..]));
        } else if let Some(i) = t.find("require(") {
            out.extend(quoted(&t[i + 8..]));
        } else if let Some(r) = t.strip_prefix("} from ") {
            out.extend(quoted(r));
        }
    }
    out
}

fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// `compilerOptions.paths` of the project's tsconfig.json: `"~/*": ["./app/*"]`
/// becomes (`~/`, `app/`).
fn tsconfig_aliases(root: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(root.join("tsconfig.json")) else {
        return vec![];
    };
    let cleaned = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    let Ok(config) = serde_json::from_str::<Value>(&cleaned) else {
        return vec![];
    };
    let mut out = Vec::new();
    if let Some(paths) = config["compilerOptions"]["paths"].as_object() {
        for (alias, targets) in paths {
            let Some(target) = targets
                .as_array()
                .and_then(|t| t.first())
                .and_then(Value::as_str)
            else {
                continue;
            };
            let (Some(a), Some(t)) = (alias.strip_suffix('*'), target.strip_suffix('*')) else {
                continue;
            };
            out.push((a.to_owned(), t.trim_start_matches("./").to_owned()));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changes_follow_the_audit() {
        let source = "import x from 'y'\nexport const a = 1\nlet b: number\nfunction f() {\n  if (a) return 2\n  return;\n}\nconst el = <div>{a && b}</div>\n";
        let starts = statement_starts("x.tsx", source);
        let js = Language::JavaScript;
        assert_eq!(change_at(&starts, 1, "import x from 'y'", js), None);
        assert_eq!(
            change_at(&starts, 2, "const a = 1", js),
            Some(Change::ValueUndefined)
        );
        assert_eq!(change_at(&starts, 3, "let b: number", js), None);
        assert_eq!(
            change_at(&starts, 5, "if (a) return 2", js),
            Some(Change::Invert)
        );
        assert_eq!(change_at(&starts, 6, "return;", js), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 8, "a && b", js),
            Some(Change::ExpressionUndefined)
        );
    }

    #[test]
    fn python_changes_follow_the_audit() {
        let source = "import os\n\n\ndef f(x):\n    \"\"\"Docs.\"\"\"\n    y = x + 1\n    if y > 2:\n        return y\n    z: int\n    count += 1\n    return\ndef g(v):\n    if v:\n        pass\n    elif v is None:\n        pass\n";
        let starts = python_statement_starts(source);
        let py = Language::Python;
        assert_eq!(change_at(&starts, 1, "import os", py), None);
        assert_eq!(change_at(&starts, 4, "def f(x):", py), None);
        assert_eq!(change_at(&starts, 5, "\"\"\"Docs.\"\"\"", py), None);
        assert_eq!(
            change_at(&starts, 6, "y = x + 1", py),
            Some(Change::ValueUndefined)
        );
        assert_eq!(change_at(&starts, 7, "if y > 2:", py), Some(Change::Invert));
        assert_eq!(
            change_at(&starts, 8, "return y", py),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(change_at(&starts, 9, "z: int", py), None);
        assert_eq!(change_at(&starts, 10, "count += 1", py), Some(Change::Skip));
        assert_eq!(change_at(&starts, 11, "return", py), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 15, "v is None", py),
            Some(Change::Invert)
        );
        assert_eq!(
            python_title("tests/test_a.py::TestA::test_b[1.5-x]"),
            "test_b"
        );
        assert_eq!(python_title("tests.test_a.TestA.test_b"), "test_b");
        assert_eq!(python_condition("if y > 2:"), "y > 2");
        assert_eq!(python_condition("elif v is None:"), "v is None");
        assert_eq!(
            python_imports(
                "from .helpers import make\nfrom . import fixtures, util as u\nimport tests.support, json as j\n"
            ),
            vec![".helpers", ".fixtures", ".util", "tests.support", "json"]
        );
    }

    #[test]
    fn change_texts() {
        assert_eq!(
            split_initializer("const x = a === b").as_deref(),
            Some("const x =")
        );
        assert_eq!(
            split_initializer("const f = () => 1").as_deref(),
            Some("const f =")
        );
        assert_eq!(condition("} else if (x > 1) {"), "x > 1");
        assert_eq!(
            condition("if (!body) return null"),
            "if (!body) return null"
        );
        assert!(is_test_call("  it(\"works\", () => {"));
        assert!(is_test_call("test.each([1])('x', () => {"));
        assert!(!is_test_call("items.push(1)"));
    }

    #[test]
    fn keys_ignore_line_numbers() {
        assert_eq!(
            strip_numbers("▶   12   return x\n        ... 3 lines not shown\n    15 }"),
            "▶  return x\n...\n }"
        );
        assert_eq!(
            strip_location("src/a.ts:41: `x` is deleted."),
            "src/a.ts:: `x` is deleted."
        );
    }
}
