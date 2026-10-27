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
use oxc_ast::ast::{
    Declaration, ExportDefaultDeclarationKind, Expression, Statement as JsStatement,
};
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
/// Asked last, when none of those is judged to fail: the tests most likely
/// written for the statement.
pub const LAST_TESTS: usize = 3;
/// The most tests one statement is asked of.
pub const MAX_TESTS: usize = MORE_TESTS + LAST_TESTS;
pub const MAX_QUESTIONS: usize = 150;
/// A statement is asserted when some test's answer is at least this.
pub const THRESHOLD: f64 = 0.5;
const HEAD_LINES: usize = 10;
const HELPER_CHARS: usize = 3000;
/// The budget for data files a test names (Ruby spec support fixtures),
/// beside the helpers': the cases a data-driven test checks are its asserts.
const FIXTURE_CHARS: usize = 12000;
const CONTEXT_LINES: usize = 0;
const WINDOW_LINES: usize = 4;
/// How far either side of a statement that is in no function its file is
/// shown.
const OUTSIDE_A_FUNCTION: usize = 25;
const CODE_CHARS: usize = 10000;
const BODY_LINES: usize = 150;

pub const INSTRUCTIONS: &str = "Each question describes exactly one change to the program, made alone. The test passes on the code as written and executes the changed line. Question: with that change, does this test fail (an assertion fails, test or helper code throws, the test times out, or the process crashes)? Follow what the changed line computes to what the test checks; do not guess from names. In `code_run`, lines marked ▶ start statements this test ran.";

fn runtime(runner: &str) -> &'static str {
    match runner {
        "node:test" => {
            "node:test; a test past its timeout fails; after hooks still run. The tests spawn the program as child processes; an unhandled promise rejection ends a Node process."
        }
        "jest" => {
            "Jest; a test past its timeout (default 5000 ms) fails; an unhandled error during the run fails it; jest.fn() returns undefined unless given an implementation. toBeNull() passes only for null and toBeUndefined() only for undefined; toBe(null), toEqual(null) and toStrictEqual(null) fail on undefined, and not.toBeUndefined() fails on it; toBeFalsy(), toBeNil-style checks, `== null` and `??` treat null and undefined alike."
        }
        "ava" => {
            "AVA; a failed t assertion, an uncaught error or a rejected promise the test returns fails it; a test past its timeout (default 10 s) fails; a test with no assertion fails unless failWithoutAssertions is off; a file's tests run concurrently unless declared with test.serial."
        }
        "tap" => {
            "node-tap; a failed t assertion, an uncaught error or a rejected promise a subtest returns fails it; a subtest past its timeout (default 30 s) fails; a parent fails when a subtest does; each test file runs as a process of its own."
        }
        "mocha" => {
            "Mocha; a test past its timeout (default 2000 ms) fails; an uncaught error, a rejected promise it returns or an error passed to done fails it; beforeEach hooks run before each test."
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
            "Vitest; a test past its timeout (default 5000 ms) fails; an unhandled error during the run fails it; vi.fn() returns undefined unless given an implementation. toBeNull() passes only for null and toBeUndefined() only for undefined; toBe(null), toEqual(null) and toStrictEqual(null) fail on undefined, and not.toBeUndefined() fails on it; toBeFalsy(), toBeNil-style checks, `== null` and `??` treat null and undefined alike."
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
    /// A boolean literal returned or declared: it becomes the other one.
    /// `false` becoming undefined changes nothing a truthiness check reads,
    /// so that change could never be caught.
    Flip,
    /// A TypeScript `throw`: it throws a bare `new Error()` instead. Deleting
    /// it seldom compiles: a throw usually guards the narrowing the next line
    /// relies on (`if ('error' in result) throw ...; return result.result`
    /// is TS2339 without it). Keeping the throw keeps the types, and asks
    /// whether any test checks which error comes out.
    BareError,
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
    /// Whether a request shows, of the code the test ran, the function each
    /// asked statement is in and nothing else, and leaves the test's helper
    /// files out. Measured for these two on eight projects against the
    /// outcome of applying each change (2026-10-13): 28 to 81% of the tokens,
    /// and 451 wrong verdicts where the fuller request gave 457. The other
    /// languages have not been measured and are asked as before.
    fn asks_within_the_function(self) -> bool {
        matches!(self, Language::JavaScript | Language::Python)
    }

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
                    // A block comment closed before code on its own line
                    // (`/** @type {number} */ const max = read();`) leaves
                    // the line code.
                    || t.strip_prefix("/*").is_some_and(|r| {
                        r.split_once("*/")
                            .is_none_or(|(_, rest)| rest.trim().is_empty() || self.comment(rest))
                    })
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
    /// Files read once: every request and every cache key reads the same text.
    files: std::sync::Mutex<BTreeMap<String, Option<std::sync::Arc<str>>>>,
    /// Whether a test file contains a text fragment, by (file, fragment).
    quotes: std::sync::Mutex<std::collections::HashMap<(String, String), bool>>,
    /// Each test's code, found once: every request and every cache key of the
    /// test shows the same code, and finding it searches the whole test file.
    test_codes: Vec<std::sync::OnceLock<String>>,
    /// The same with line numbers taken out, as cache keys hold it.
    unnumbered_test_codes: Vec<std::sync::OnceLock<String>>,
    /// Each file's lines, comments and functions, worked out once: every
    /// request and every cache key shows code of the same files.
    views: std::sync::Mutex<BTreeMap<String, std::sync::Arc<FileView>>>,
}

/// A source file as the requests show it: its lines from one, which are
/// comments, and the functions and statement starts asked about within it.
struct FileView {
    source: std::sync::Arc<str>,
    /// Where each line starts, and one past the end of the file.
    starts: Vec<usize>,
    /// By line, from one at index zero.
    comment: Vec<bool>,
    /// The lines that are code, from one.
    code: Vec<usize>,
    functions: std::sync::OnceLock<Vec<(usize, usize)>>,
    begins: std::sync::OnceLock<BTreeSet<usize>>,
}

impl FileView {
    fn new(language: Language, source: std::sync::Arc<str>) -> FileView {
        let mut starts = vec![0];
        starts.extend(source.match_indices('\n').map(|(i, _)| i + 1));
        starts.push(source.len() + 1);
        let comment = (1..starts.len())
            .map(|i| language.comment(&source[starts[i - 1]..starts[i] - 1]))
            .collect::<Vec<_>>();
        let code = (1..=comment.len()).filter(|&i| !comment[i - 1]).collect();
        FileView {
            source,
            starts,
            comment,
            code,
            functions: Default::default(),
            begins: Default::default(),
        }
    }

    fn len(&self) -> usize {
        self.comment.len()
    }

    /// A line, from one.
    fn line(&self, line: usize) -> &str {
        &self.source[self.starts[line - 1]..self.starts[line] - 1]
    }

    /// Where a line is among the code lines; none for a comment.
    fn position(&self, line: usize) -> Option<usize> {
        self.code.binary_search(&line).ok()
    }

    fn functions(&self, language: Language, file: &str) -> &[(usize, usize)] {
        self.functions
            .get_or_init(|| outermost_functions(language, file, &self.source))
    }

    fn begins(&self, find: impl FnOnce() -> BTreeSet<usize>) -> &BTreeSet<usize> {
        self.begins.get_or_init(find)
    }
}

/// The executed statements of a run and the tests that ran them. `sources` are
/// the run's source files, verified against the run.
/// What assessing a run reads from its analysis: the passing view's tests,
/// and its measured statements and executed decisions with the tests that ran
/// them. Publication stores it, so an assessment does not analyse the run's
/// evidence again (4.4 s of supergateway's 7.6 s of planning).
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct AssessmentInput {
    pub tests: Vec<InputTest>,
    pub points: Vec<InputPoint>,
    pub decisions: Vec<InputPoint>,
}

impl AssessmentInput {
    /// Only the tests `keep` picks, and the statements and decisions they ran:
    /// what assessing some of a run's tests asks about.
    pub fn only_tests(&self, keep: impl Fn(&InputTest) -> bool) -> AssessmentInput {
        let mut renumbered = vec![None; self.tests.len()];
        let mut tests = Vec::new();
        for (index, test) in self.tests.iter().enumerate() {
            if keep(test) {
                renumbered[index] = Some(tests.len() as u32);
                tests.push(test.clone());
            }
        }
        let narrow = |points: &[InputPoint]| {
            points
                .iter()
                .filter_map(|point| {
                    let tests = point
                        .tests
                        .iter()
                        .filter_map(|test| renumbered.get(*test as usize).copied().flatten())
                        .collect::<Vec<_>>();
                    (!tests.is_empty()).then(|| InputPoint {
                        tests,
                        ..point.clone()
                    })
                })
                .collect()
        };
        AssessmentInput {
            points: narrow(&self.points),
            decisions: narrow(&self.decisions),
            tests,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct InputTest {
    pub id: String,
    pub file: Option<String>,
    pub name: String,
    pub role: String,
    pub runner: String,
}

/// A statement or decision, with the tests that ran it as indexes into
/// `AssessmentInput::tests`.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub struct InputPoint {
    pub file: String,
    pub line: usize,
    pub source: String,
    pub tests: Vec<u32>,
}

pub fn assessment_input(report: &CoverageReport) -> AssessmentInput {
    let view = &report.filters.passed;
    let tests = view
        .tests
        .iter()
        .map(|test| InputTest {
            id: test.id.clone(),
            file: test.file.clone(),
            name: test.name.clone(),
            role: test.role.clone(),
            runner: test.provenance.runner.clone(),
        })
        .collect::<Vec<_>>();
    let index = tests
        .iter()
        .enumerate()
        .map(|(i, test)| (test.id.as_str(), i as u32))
        .collect::<BTreeMap<_, _>>();
    let indexes = |ids: &[crate::interned::Id]| {
        ids.iter()
            .filter_map(|id| index.get(id.as_str()).copied())
            .collect::<Vec<_>>()
    };
    let points = view
        .points
        .iter()
        // A view leaves out every point the run declined, so each is measured.
        .filter(|p| p.meta.kind == PointKind::Statement)
        .map(|p| InputPoint {
            file: p.meta.file.clone(),
            line: p.meta.line,
            source: p.meta.source.clone(),
            tests: indexes(&p.tests),
        })
        .collect();
    let decisions = view
        .decisions
        .iter()
        .filter(|d| d.executed)
        .map(|d| InputPoint {
            file: d.meta.file.clone(),
            line: d.meta.line,
            source: d.meta.source.clone(),
            tests: indexes(&d.tests),
        })
        .collect();
    AssessmentInput {
        tests,
        points,
        decisions,
    }
}

pub fn population_from(
    root: &Path,
    input: &AssessmentInput,
    sources: &BTreeMap<String, String>,
    language: Language,
) -> Result<Population, String> {
    let mut tests = Vec::new();
    // JVM tests are named by class; their file is found by its path.
    let jvm_files = if language == Language::Jvm {
        project_files(root, &["java", "kt"])
    } else {
        Vec::new()
    };
    // Input test index -> population test index.
    let mut index = BTreeMap::<u32, usize>::new();
    for (position, test) in input.tests.iter().enumerate() {
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
        index.insert(position as u32, tests.len());
        tests.push(TestRef {
            id: test.id.clone(),
            file,
            name: test.name.clone(),
            runner: test.runner.clone(),
        });
    }
    let mut changes = BTreeMap::<String, BTreeMap<usize, Vec<(String, Option<Change>)>>>::new();
    let mut replacements = BTreeMap::<String, rust_source::Replacements>::new();
    let signatures = if language == Language::Rust {
        rust_source::signatures(
            sources
                .iter()
                .filter(|(file, _)| file.ends_with(".rs"))
                .map(|(file, source)| (file.as_str(), source.as_str())),
        )
    } else {
        rust_source::Signatures::default()
    };
    let mut statements = Vec::new();
    let mut seen = BTreeSet::new();
    // Python and Ruby record an `elif`/`elsif`, and Go, Rust and the JVM
    // languages may record an `else if`, as a decision only: its condition is
    // taken after every statement, where no statement starts on its line.
    let executed = input
        .points
        .iter()
        .map(|p| (&p.file, p.line, &p.source, &p.tests, false));
    let elifs = input
        .decisions
        .iter()
        .filter(|_| language != Language::JavaScript)
        .map(|d| (&d.file, d.line, &d.source, &d.tests, true));
    for (file, line, text, point_tests, decision) in executed.chain(elifs) {
        let key = (file.clone(), line);
        if !seen.insert(key) {
            continue; // the first statement on a line, as the audit reads it
        }
        let ids = point_tests
            .iter()
            .filter_map(|position| index.get(position).copied())
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
        let valued = matches!(change, Change::ReturnUndefined | Change::ValueUndefined);
        let replacement = match language {
            Language::Rust if valued => {
                let first = prefix(text.lines().next().unwrap_or("").trim(), 30);
                let first = first.trim_end_matches(';');
                replacements
                    .entry(file.clone())
                    // A bug reading one file's values costs its questions
                    // their replacements, which then describe the change.
                    .or_insert_with(|| {
                        crate::contained::contained(|| {
                            crate::fault::or(
                                "value-changes-panic",
                                || panic!("injected fault at value-changes-panic"),
                                || rust_source::replacements(file, source, &signatures),
                            )
                        })
                        .unwrap_or_default()
                    })
                    .get(&line)
                    .and_then(|all| all.iter().find(|(t, _)| t.starts_with(first)))
                    .map(|(_, r)| r.clone())
            }
            // Go: the line the change turns it into, zero values written out
            // from the result types. (Writing the JVM's `null` out the same
            // way measured no better on commons-cli and moved the share away
            // from the truth, so the JVM keeps its description.)
            Language::Go if valued => go::rewrite(source, line, change),
            _ => None,
        };
        // A replacement that writes the statement out as it already is --
        // `return 0, nil` with zero values, `count := 0` -- is no change.
        if replacement
            .as_deref()
            .is_some_and(|written| same_code(written, text))
        {
            continue;
        }
        statements.push(Statement {
            file: file.clone(),
            line,
            text: text.clone(),
            change,
            replacement,
            tests: ids,
        });
    }
    if matches!(language, Language::JavaScript | Language::Python) {
        credit_module_level(&mut statements, sources, language);
    }
    let mut ran = vec![BTreeMap::<String, BTreeSet<usize>>::new(); tests.len()];
    for s in &statements {
        for &t in &s.tests {
            ran[t].entry(s.file.clone()).or_default().insert(s.line);
        }
    }
    let source_files = statements.iter().map(|s| s.file.clone()).collect();
    let test_codes = tests.iter().map(|_| std::sync::OnceLock::new()).collect();
    let unnumbered_test_codes = tests.iter().map(|_| std::sync::OnceLock::new()).collect();
    Ok(Population {
        root: root.to_path_buf(),
        language,
        statements,
        tests,
        ran,
        source_files,
        jvm_files,
        files: Default::default(),
        quotes: Default::default(),
        test_codes,
        unnumbered_test_codes,
        views: Default::default(),
    })
}

/// A statement at a module's top level runs once, when the module is first
/// loaded, and is recorded for whichever test loaded it -- often one an auth
/// gate turns away -- so only that test was asked about it. Every test that
/// runs the module's functions relies on it: they are credited too, after the
/// tests recorded for it.
fn credit_module_level(
    statements: &mut [Statement],
    sources: &BTreeMap<String, String>,
    language: Language,
) {
    let mut files = BTreeMap::<String, Vec<usize>>::new();
    for (index, statement) in statements.iter().enumerate() {
        files.entry(statement.file.clone()).or_default().push(index);
    }
    for (file, indexes) in files {
        let Some(source) = sources.get(&file) else {
            continue;
        };
        let functions = outermost_functions(language, &file, source);
        let inside = |line: usize| {
            functions
                .iter()
                .any(|&(first, last)| first <= line && line <= last)
        };
        let users = indexes
            .iter()
            .filter(|&&index| inside(statements[index].line))
            .flat_map(|&index| statements[index].tests.iter().copied())
            .collect::<BTreeSet<_>>();
        let module_level = indexes
            .iter()
            .copied()
            .filter(|&index| !inside(statements[index].line))
            .collect::<Vec<_>>();
        for index in module_level {
            let tests = &mut statements[index].tests;
            for &user in &users {
                if !tests.contains(&user) {
                    tests.push(user);
                }
            }
        }
    }
}

/// Whether two lines of code are the same but for spacing and a final `;`.
fn same_code(a: &str, b: &str) -> bool {
    let squash = |code: &str| {
        code.trim()
            .trim_end_matches(';')
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect::<String>()
    };
    squash(a) == squash(b)
}

/// The words of a name or a line of code: runs of letters and digits split at
/// camelCase and at anything else, lowercased, short and filler words left out.
fn name_words(text: &str) -> BTreeSet<String> {
    const FILLER: [&str; 13] = [
        "test", "tests", "the", "and", "for", "with", "get", "set", "self", "from", "not", "does",
        "should",
    ];
    let mut words = BTreeSet::new();
    let chars = text.chars().collect::<Vec<_>>();
    let mut word = String::new();
    let mut flush = |word: &mut String| {
        let w = word.to_lowercase();
        if w.len() > 2 && !FILLER.contains(&w.as_str()) {
            words.insert(w);
        }
        word.clear();
    };
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            flush(&mut word);
            continue;
        }
        let previous = i.checked_sub(1).map(|p| chars[p]);
        let next = chars.get(i + 1).copied();
        // `getHTTPResponse`: a word starts at a capital after a lowercase
        // letter, and at the last capital of a run before a lowercase one.
        let starts = c.is_ascii_uppercase()
            && (previous.is_some_and(|p| p.is_ascii_lowercase() || p.is_ascii_digit())
                || (previous.is_some_and(|p| p.is_ascii_uppercase())
                    && next.is_some_and(|n| n.is_ascii_lowercase())));
        if starts {
            flush(&mut word);
        }
        word.push(c);
    }
    flush(&mut word);
    words
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
        typescript: bool,
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
            self.out.entry(line).or_default().push((
                text,
                match statement {
                    JsStatement::ThrowStatement(_) if self.typescript => Some(Change::BareError),
                    _ => change_of(statement),
                },
            ));
            walk::walk_statement(self, statement);
        }
    }
    let mut collector = Collector {
        source,
        starts: &starts,
        typescript: source_type.is_typescript(),
        out: BTreeMap::new(),
    };
    collector.visit_program(&parsed.program);
    collector.out
}

/// `undefined` or `void 0`: returning undefined instead is the same code.
/// What TypeScript wraps a value in to give it a type (`undefined as T`)
/// leaves it the same value.
fn is_undefined(expression: &Expression) -> bool {
    match expression {
        Expression::Identifier(identifier) => identifier.name == "undefined",
        Expression::UnaryExpression(unary) => {
            unary.operator == oxc_syntax::operator::UnaryOperator::Void
        }
        Expression::TSAsExpression(cast) => is_undefined(&cast.expression),
        Expression::TSSatisfiesExpression(cast) => is_undefined(&cast.expression),
        Expression::TSTypeAssertion(cast) => is_undefined(&cast.expression),
        Expression::TSNonNullExpression(cast) => is_undefined(&cast.expression),
        Expression::ParenthesizedExpression(inner) => is_undefined(&inner.expression),
        _ => false,
    }
}

fn change_of(statement: &JsStatement) -> Option<Change> {
    Some(match statement {
        JsStatement::IfStatement(_) => Change::Invert,
        // `return undefined` returning undefined is the same code; skipping
        // the return is the change that can show.
        JsStatement::ReturnStatement(r) => match &r.argument {
            Some(argument) if is_undefined(argument) => Change::Skip,
            Some(Expression::BooleanLiteral(_)) => Change::Flip,
            Some(_) => Change::ReturnUndefined,
            None => Change::Skip,
        },
        JsStatement::VariableDeclaration(d) => {
            if let [only] = d.declarations.as_slice()
                && matches!(only.init, Some(Expression::BooleanLiteral(_)))
            {
                Change::Flip
            } else if d.declarations.iter().any(|d| d.init.is_some()) {
                Change::ValueUndefined
            } else {
                return None;
            }
        }
        // A constructor cannot skip `super(...)`: the result does not compile
        // (TS2377), and in JavaScript it throws before anything is observed.
        JsStatement::ExpressionStatement(e)
            if matches!(&e.expression, Expression::CallExpression(call)
                if matches!(call.callee, Expression::Super(_))) =>
        {
            return None;
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
                Stmt::Return(r) => Some(match r.value.as_deref() {
                    // `return None` returning None is the same code.
                    Some(Expr::NoneLiteral(_)) | None => Change::Skip,
                    Some(Expr::BooleanLiteral(_)) => Change::Flip,
                    Some(_) => Change::ReturnUndefined,
                }),
                Stmt::Assign(a)
                    if a.targets.len() == 1
                        && matches!(a.value.as_ref(), Expr::BooleanLiteral(_)) =>
                {
                    Some(Change::Flip)
                }
                Stmt::Assign(_) => Some(Change::ValueUndefined),
                Stmt::AnnAssign(a)
                    if matches!(a.value.as_deref(), Some(Expr::BooleanLiteral(_))) =>
                {
                    Some(Change::Flip)
                }
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

/// The line a JavaScript test is declared on. Each group in its name
/// (`cart > totals`) narrows the search to that group's block, since groups
/// often hold tests of the same title; a title written as a string is
/// preferred over the same words anywhere else.
fn js_test_line(lines: &[&str], name: &str) -> Option<usize> {
    let parts = name.split(" > ").collect::<Vec<_>>();
    let (title, groups) = parts
        .split_last()
        .expect("a split's first piece is always there");
    let quoted = |line: &str, text: &str| {
        ['\'', '"', '`']
            .iter()
            .any(|quote| line.contains(&format!("{quote}{text}")))
    };
    let (mut from, mut to) = (0, lines.len());
    for group in groups {
        let needle = prefix(group, 40);
        if needle.is_empty() {
            continue;
        }
        if let Some(at) = (from..to).find(|&i| quoted(lines[i], needle)) {
            (from, to) = (at, (block_end(lines, at) + 1).min(lines.len()));
        }
    }
    let needle = prefix(title, 40);
    if needle.is_empty() {
        return None;
    }
    (from..to)
        .find(|&i| quoted(lines[i], needle))
        .or_else(|| (from..to).find(|&i| lines[i].contains(needle)))
        .or_else(|| (0..lines.len()).find(|&i| lines[i].contains(needle)))
}

fn prefix(text: &str, chars: usize) -> &str {
    match text.char_indices().nth(chars) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// The lines of each function that lies in no other, with the statement that
/// declares it (`export const seal = (key) => {`) and its decorators. A
/// callback written inside a function belongs to it. First and last line,
/// from one.
fn outermost_functions(language: Language, file: &str, source: &str) -> Vec<(usize, usize)> {
    let starts = line_starts(source);
    let lines = |from: usize, to: usize| {
        (
            line_of(&starts, from),
            line_of(&starts, to.saturating_sub(1).max(from)),
        )
    };
    let mut spans = match language {
        Language::JavaScript => {
            let allocator = Allocator::default();
            let source_type = SourceType::from_path(file).unwrap_or_else(|_| SourceType::tsx());
            let parsed = Parser::new(&allocator, source, source_type).parse();
            #[derive(Default)]
            struct Functions {
                // The statement, or the member of a class, a function is
                // written in: what stands directly in the file, a block or a
                // class body.
                within: Option<(u32, u32)>,
                found: Vec<(u32, u32)>,
            }
            impl Functions {
                fn inside(&mut self, span: (u32, u32), walk: impl FnOnce(&mut Self)) {
                    let outer = self.within.replace(span);
                    walk(self);
                    self.within = outer;
                }
                fn function(&mut self, own: oxc_span::Span) {
                    self.found.push(self.within.unwrap_or((own.start, own.end)));
                }
            }
            fn from_decorators(
                span: oxc_span::Span,
                decorators: &[oxc_ast::ast::Decorator<'_>],
            ) -> (u32, u32) {
                (
                    decorators
                        .first()
                        .map_or(span.start, |first| first.span.start.min(span.start)),
                    span.end,
                )
            }
            impl<'a> Visit<'a> for Functions {
                fn visit_program(&mut self, program: &oxc_ast::ast::Program<'a>) {
                    for statement in &program.body {
                        let span = statement.span();
                        self.inside((span.start, span.end), |this| {
                            this.visit_statement(statement)
                        });
                    }
                }
                fn visit_block_statement(&mut self, block: &oxc_ast::ast::BlockStatement<'a>) {
                    for statement in &block.body {
                        let span = statement.span();
                        self.inside((span.start, span.end), |this| {
                            this.visit_statement(statement)
                        });
                    }
                }
                fn visit_ts_module_block(&mut self, block: &oxc_ast::ast::TSModuleBlock<'a>) {
                    for statement in &block.body {
                        let span = statement.span();
                        self.inside((span.start, span.end), |this| {
                            this.visit_statement(statement)
                        });
                    }
                }
                fn visit_method_definition(&mut self, it: &oxc_ast::ast::MethodDefinition<'a>) {
                    self.inside(from_decorators(it.span, &it.decorators), |this| {
                        walk::walk_method_definition(this, it)
                    });
                }
                fn visit_property_definition(&mut self, it: &oxc_ast::ast::PropertyDefinition<'a>) {
                    self.inside(from_decorators(it.span, &it.decorators), |this| {
                        walk::walk_property_definition(this, it)
                    });
                }
                fn visit_accessor_property(&mut self, it: &oxc_ast::ast::AccessorProperty<'a>) {
                    self.inside(from_decorators(it.span, &it.decorators), |this| {
                        walk::walk_accessor_property(this, it)
                    });
                }
                fn visit_static_block(&mut self, it: &oxc_ast::ast::StaticBlock<'a>) {
                    self.inside((it.span.start, it.span.end), |this| {
                        walk::walk_static_block(this, it)
                    });
                }
                // Not walked into: what is inside a function is the function's.
                fn visit_function(
                    &mut self,
                    it: &oxc_ast::ast::Function<'a>,
                    _flags: oxc_syntax::scope::ScopeFlags,
                ) {
                    if it.body.is_some() {
                        self.function(it.span);
                    }
                }
                fn visit_arrow_function_expression(
                    &mut self,
                    it: &oxc_ast::ast::ArrowFunctionExpression<'a>,
                ) {
                    self.function(it.span);
                }
            }
            let mut functions = Functions::default();
            functions.visit_program(&parsed.program);
            functions
                .found
                .into_iter()
                .map(|(from, to)| lines(from as usize, to as usize))
                .collect::<Vec<_>>()
        }
        Language::Python => {
            use ruff_python_ast::{
                Stmt,
                visitor::{Visitor, walk_stmt},
            };
            use ruff_text_size::Ranged;
            struct Functions(Vec<(usize, usize)>);
            impl<'a> Visitor<'a> for Functions {
                fn visit_stmt(&mut self, stmt: &'a Stmt) {
                    match stmt {
                        Stmt::FunctionDef(function) => {
                            let from = function
                                .decorator_list
                                .first()
                                .map_or(function.start(), |first| {
                                    first.start().min(function.start())
                                });
                            self.0.push((from.to_usize(), function.end().to_usize()));
                        }
                        _ => walk_stmt(self, stmt),
                    }
                }
            }
            let Ok(parsed) = ruff_python_parser::parse_module(source) else {
                return Vec::new();
            };
            let mut functions = Functions(Vec::new());
            for stmt in &parsed.syntax().body {
                functions.visit_stmt(stmt);
            }
            functions
                .0
                .into_iter()
                .map(|(from, to)| lines(from, to))
                .collect()
        }
        _ => Vec::new(),
    };
    spans.sort_unstable();
    spans.dedup();
    spans
}

/// How many lines a deleted statement is named by, when its question names
/// all of them ("all 4 lines of it (lines 133 to 136)").
fn whole_statement(statement: &Statement) -> Option<usize> {
    let lines = statement.text.lines().count();
    let word = statement
        .text
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches(';')
        .trim();
    (statement.change == Change::Skip
        && lines > 1
        && !matches!(word, "break" | "continue" | "next"))
    .then_some(lines)
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

/// The text a statement writes out: the fixed parts of its string literals,
/// six characters or longer with a letter in them (`- Headers:` from
/// `` `  - Headers: ${describe(headers)}` ``). A test that quotes one is the
/// likeliest to assert what the statement produces.
fn quoted_fragments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some((start, quote)) = chars.next() {
        if !matches!(quote, '\'' | '"' | '`') {
            continue;
        }
        let mut end = None;
        while let Some((at, c)) = chars.next() {
            if c == '\\' {
                chars.next();
            } else if c == quote {
                end = Some(at);
                break;
            }
        }
        let Some(end) = end else { break };
        let body = &text[start + quote.len_utf8()..end];
        // The fixed text only: what sits inside `${...}`, `#{...}` or an
        // f-string's `{...}` is an expression, not output.
        let mut pieces = vec![String::new()];
        let mut depth = 0usize;
        for c in body.chars() {
            match c {
                '{' => {
                    depth += 1;
                    pieces.push(String::new());
                }
                '}' if depth > 0 => depth -= 1,
                '$' | '#' | '%' if depth == 0 => pieces.push(String::new()),
                _ if depth == 0 => pieces.last_mut().expect("one piece").push(c),
                _ => {}
            }
        }
        out.extend(
            pieces
                .iter()
                .map(|piece| piece.trim())
                .filter(|piece| {
                    piece.len() >= 6 && piece.chars().filter(|c| c.is_alphabetic()).count() >= 3
                })
                .map(str::to_owned),
        );
    }
    out.sort();
    out.dedup();
    out
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
    pub fn with_files(mut self, files: BTreeMap<String, String>) -> Self {
        {
            let mut cache = self
                .files
                .lock()
                .expect("the file cache is only read and filled");
            for (file, text) in files {
                cache.insert(file, Some(text.into()));
            }
        }
        // Test code found before is code of the files replaced.
        for code in self
            .test_codes
            .iter_mut()
            .chain(&mut self.unnumbered_test_codes)
        {
            *code = std::sync::OnceLock::new();
        }
        self.views = Default::default();
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
                crate::fault_fs::read_to_string(self.root.join(file))
                    .ok()
                    .map(Into::into)
            })
            .clone()
    }

    fn view(&self, file: &str) -> Option<std::sync::Arc<FileView>> {
        let source = self.read(file)?;
        let mut views = self
            .views
            .lock()
            .expect("the view cache is only read and filled");
        Some(
            views
                .entry(file.to_owned())
                .or_insert_with(|| std::sync::Arc::new(FileView::new(self.language, source)))
                .clone(),
        )
    }

    /// Whether `test`'s file contains one of these fragments.
    fn quotes(&self, test: usize, fragments: &[String]) -> bool {
        let file = &self.tests[test].file;
        fragments.iter().any(|fragment| {
            let key = (file.clone(), fragment.clone());
            if let Some(&known) = self
                .quotes
                .lock()
                .expect("the quote cache is only read and filled")
                .get(&key)
            {
                return known;
            }
            let found = self
                .read(file)
                .is_some_and(|text| text.contains(fragment.as_str()));
            self.quotes
                .lock()
                .expect("the quote cache is only read and filled")
                .insert(key, found);
            found
        })
    }

    /// The tests to ask about a statement, in order: tests whose file quotes
    /// text the statement produces first, one per file; then tests in a file
    /// named like the source; then one per describe block, round-robin over
    /// files. Without the first rule, the alphabetically first files filled
    /// the sample: on supergateway 15 of 179 tests were asked about a log
    /// line, none of them the test that compares the whole startup log.
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
        all.sort_by_cached_key(key);
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
        let fragments = quoted_fragments(&s.text);
        if !fragments.is_empty() {
            let quoting = all
                .iter()
                .copied()
                .filter(|&t| self.quotes(t, &fragments))
                .collect::<Vec<_>>();
            let mut files = BTreeSet::new();
            let (first, rest): (Vec<usize>, Vec<usize>) = quoting
                .iter()
                .partition(|&&t| files.insert(self.tests[t].file.clone()));
            for t in first.into_iter().chain(rest) {
                if picked.len() < k {
                    picked.push(t);
                }
            }
        }
        let mut depth = 0;
        while picked.len() < k && by_file.iter().any(|(_, g)| g.len() > depth) {
            for (_, g) in &by_file {
                if depth < g.len() && picked.len() < k && !picked.contains(&g[depth][0]) {
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

    /// The tests to ask last, when none of `asked` was judged to fail: of the
    /// rest, those whose name shares the most words with the statement's
    /// line, and among those the tests that ran the least code.
    ///
    /// A statement hundreds of tests run is mostly run in passing, by
    /// end-to-end tests on their way somewhere else. The tests written for it
    /// tend to say so in their name (`corsOrigin: a false value` for
    /// `return corsOrigin(...)`) and to run little besides it. Asking a few
    /// more in the usual order found few of them; replacing some of the usual
    /// tests with these lost statements the usual tests catch.
    pub fn last_order(&self, statement: usize, asked: &[usize], k: usize) -> Vec<usize> {
        let spread = self.order(statement, MORE_TESTS);
        let position = |t: &usize| spread.iter().position(|x| x == t).unwrap_or(usize::MAX);
        let s = &self.statements[statement];
        let line = name_words(s.text.lines().next().unwrap_or(""));
        let shared = |t: usize| {
            let name = &self.tests[t].name;
            let name = name.rsplit("::").next().unwrap_or(name);
            name_words(name).intersection(&line).count()
        };
        let mut rest = s
            .tests
            .iter()
            .copied()
            .filter(|t| !asked.contains(t))
            .collect::<Vec<_>>();
        rest.sort_by_cached_key(|t| {
            (
                std::cmp::Reverse(shared(*t)),
                self.breadth(*t),
                position(t),
                *t,
            )
        });
        rest.truncate(k);
        rest
    }

    /// How many statement lines a test ran.
    fn breadth(&self, test: usize) -> usize {
        self.ran[test].values().map(BTreeSet::len).sum()
    }

    /// The test's code: the file's first lines and the test's own body, found
    /// by its title, comment lines left out.
    pub fn test_code(&self, test: usize) -> String {
        self.test_codes[test]
            .get_or_init(|| self.find_test_code(test))
            .clone()
    }

    fn find_test_code(&self, test: usize) -> String {
        match self.language {
            Language::Python => return self.python_test_code(test),
            Language::JavaScript => {}
            _ => return self.located_test_code(test),
        }
        let t = &self.tests[test];
        let text = self.read(&t.file).unwrap_or_default();
        let lines = text.split('\n').collect::<Vec<_>>();
        let start = js_test_line(&lines, &t.name);
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
                        return was || self.language.comment(l);
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
        // Test classes in one file often share method names, so the method
        // is looked for inside the class its id names, then anywhere.
        let (from, to, body) = python_class_lines(&lines, &python_classes(&t.name));
        let defines = |i: &usize| {
            let t = lines[*i].trim_start();
            t.starts_with(&format!("def {title}(")) || t.starts_with(&format!("async def {title}("))
        };
        // In the class's own body, not a class nested in it.
        let own = |i: &usize| defines(i) && body.is_none_or(|body| indent_of(lines[*i]) == body);
        let start = (!title.is_empty())
            .then(|| {
                (from..to)
                    .find(own)
                    .or_else(|| (0..lines.len()).find(defines))
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
        // `...` where lines are left out between the ones shown.
        let mut last = HEAD_LINES.min(lines.len()).checked_sub(1);
        let mut show = |out: &mut Vec<String>, i: usize| {
            if i >= HEAD_LINES {
                if last.is_some_and(|l| i > l + 1) {
                    out.push("  ...".into());
                }
                out.push(fmt(i));
                last = Some(i);
            }
        };
        for i in class.into_iter().chain(first..=start) {
            show(&mut out, i);
        }
        for (i, &l) in lines
            .iter()
            .enumerate()
            .skip(start + 1)
            .take(BODY_LINES - 1)
        {
            if !l.trim().is_empty() && indent_of(l) <= indent {
                break;
            }
            // A comment in the body is left out without a mark.
            if !self.language.comment(l) && i >= HEAD_LINES {
                out.push(fmt(i));
            }
        }
        out.join("\n")
    }

    /// Test-side files the test imports (helpers, fixtures), cut to a budget
    /// that grows with the number of questions. None for JavaScript,
    /// TypeScript and Python (see `asks_within_the_function`): on
    /// supergateway they were a sixth of what was sent and changed 4 verdicts
    /// in 101, two each way.
    pub fn helpers(&self, test: usize, questions: usize) -> BTreeMap<String, String> {
        if self.language.asks_within_the_function() {
            return BTreeMap::new();
        }
        let cap = HELPER_CHARS.min(1500 + 400 * questions);
        let t = &self.tests[test];
        let text = self.read(&t.file).unwrap_or_default();
        let mut out = BTreeMap::new();
        let mut size = 0;
        let files = match self.language {
            Language::JavaScript | Language::Python => Vec::new(),
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

    /// The source this test ran: every statement it executed marked ▶, four
    /// code lines around each asked statement, comment lines left out, within
    /// a budget that grows with the number of questions. Two lines around
    /// every executed statement and eight around each asked one, in a budget
    /// four times this, cost 28% more over 3,532 checked statements in 14
    /// projects and were no more accurate.
    ///
    /// In JavaScript, TypeScript and Python that is cut to the function each
    /// asked statement is in (see `asks_within_the_function`): what the test
    /// ran elsewhere in the file, and in other files, is left out.
    pub fn code_run(&self, test: usize, asked: &[usize]) -> BTreeMap<String, String> {
        let budget = CODE_CHARS.min(1000 + 400 * asked.len());
        let within = self.language.asks_within_the_function();
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
            .filter(|(f, _)| !within && !want.contains_key(*f))
            .collect::<Vec<_>>();
        others.sort_by_key(|(f, lines)| (std::cmp::Reverse(lines.len()), (*f).clone()));
        files.extend(others.into_iter().map(|(f, _)| f.clone()));
        let empty = BTreeSet::new();
        let mut out = BTreeMap::new();
        let mut total = 0;
        for f in files {
            let Some(view) = self.view(&f) else {
                continue;
            };
            let r = ran.get(&f).unwrap_or(&empty);
            let around = |l: usize, w: usize| -> Vec<usize> {
                match view.position(l) {
                    Some(n) => {
                        view.code[n.saturating_sub(w)..(n + w + 1).min(view.code.len())].to_vec()
                    }
                    None => (l.saturating_sub(w).max(1)..=(l + w).min(view.len())).collect(),
                }
            };
            // The last line of `around(l, w)` that is code: how far it shows
            // the file, without listing it.
            let reach = |l: usize, w: usize| -> usize {
                match view.position(l) {
                    Some(n) => view.code[(n + w).min(view.code.len() - 1)],
                    None => (l.saturating_sub(w).max(1)..=(l + w).min(view.len()))
                        .rev()
                        .find(|&i| !view.comment[i - 1])
                        .unwrap_or(0),
                }
            };
            // Within a function, only what lies in the functions kept is
            // shown: the lines the test ran elsewhere in a large file are
            // never listed (each request listed them all, then dropped them).
            let kept = within.then(|| {
                let functions = view.functions(self.language, &f);
                let around =
                    |l: usize| (l.saturating_sub(OUTSIDE_A_FUNCTION), l + OUTSIDE_A_FUNCTION);
                let function = |l: usize| {
                    functions
                        .iter()
                        .copied()
                        .find(|&(from, to)| from <= l && l <= to)
                };
                // The lines an assessed statement of this file begins on.
                let begins = view.begins(|| {
                    self.statements
                        .iter()
                        .filter(|s| s.file == f)
                        .map(|s| s.line)
                        .collect()
                });
                let mut kept = Vec::new();
                for statement in asked.iter().map(|&a| &self.statements[a]) {
                    if statement.file != f {
                        continue;
                    }
                    kept.push(function(statement.line).unwrap_or(around(statement.line)));
                    // A statement its question names by all its lines also
                    // keeps what lies near each line it goes on to, as the
                    // requests this was measured with did.
                    for l in (statement.line + 1)
                        ..statement.line + whole_statement(statement).unwrap_or(1)
                    {
                        kept.push(
                            begins
                                .contains(&l)
                                .then(|| function(l))
                                .flatten()
                                .unwrap_or(around(l)),
                        );
                    }
                }
                kept
            });
            let mut show = BTreeSet::new();
            match &kept {
                None => {
                    for &l in r {
                        show.extend(around(l, CONTEXT_LINES));
                    }
                }
                Some(kept) => {
                    // A ran line whose context can reach a kept range: within
                    // CONTEXT_LINES of its code lines, or of its lines.
                    for &(from, to) in kept {
                        let first = view.code.partition_point(|&c| c < from);
                        let last = view.code.partition_point(|&c| c <= to);
                        let lo = view
                            .code
                            .get(first.saturating_sub(CONTEXT_LINES))
                            .copied()
                            .unwrap_or(usize::MAX)
                            .min(from.saturating_sub(CONTEXT_LINES));
                        let hi = (last + CONTEXT_LINES)
                            .checked_sub(1)
                            .and_then(|n| view.code.get(n.min(view.code.len().saturating_sub(1))))
                            .copied()
                            .unwrap_or(0)
                            .max(to.saturating_add(CONTEXT_LINES));
                        // A kept range runs forward, so `lo` is at most `hi`.
                        for &l in r.range(lo..=hi) {
                            show.extend(around(l, CONTEXT_LINES));
                        }
                    }
                }
            }
            for &l in want.get(&f).unwrap_or(&empty) {
                show.extend(around(l, WINDOW_LINES));
            }
            // How far the file was going to be shown, for the line that says
            // how much of it is not. Every line `around` gives is one of the
            // file's, counted from one.
            // No line before a ran code line reaches further than it does.
            let mut reached = 0;
            for &l in r.iter().rev() {
                reached = reached.max(reach(l, CONTEXT_LINES));
                if view.position(l).is_some() {
                    break;
                }
            }
            let end = std::iter::once(reached)
                .chain(
                    want.get(&f)
                        .unwrap_or(&empty)
                        .iter()
                        .map(|&l| reach(l, WINDOW_LINES)),
                )
                .max()
                .unwrap_or(0);
            if let Some(kept) = &kept {
                show.retain(|i| kept.iter().any(|&(from, to)| from <= *i && *i <= to));
            }
            let mut lines = Vec::new();
            let mut last = 0;
            for i in show {
                if view.comment[i - 1] {
                    continue;
                }
                if i != last + 1 {
                    lines.push(format!("        ... {} lines not shown", i - last - 1));
                }
                lines.push(format!(
                    "{}{:5} {}",
                    if r.contains(&i) { '▶' } else { ' ' },
                    i,
                    view.line(i)
                ));
                last = i;
            }
            if within && last != 0 && end > last {
                lines.push(format!("        ... {} lines not shown", end - last));
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
            // The value is replaced after it is computed, as Go's mutation
            // testers do.
            (Change::ReturnUndefined, Some(r)) if self.language == Language::Go => {
                vec![format!(
                    "`{line}` becomes `{r}` (the original expression is still evaluated first)"
                )]
            }
            (Change::ValueUndefined, Some(r)) if self.language == Language::Go => {
                vec![format!(
                    "`{line}` becomes `{r}` (the original value is still computed first)"
                )]
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
            // A `when`, `switch` or `try` ending a function returns from its
            // branches.
            Change::ReturnUndefined
                if self.language == Language::Jvm
                    && s.text.contains("return")
                    && ["when", "switch", "try"].iter().any(|k| {
                        line.strip_prefix(k)
                            .is_some_and(|rest| rest.starts_with([' ', '(', '{']))
                    }) =>
            {
                vec![format!(
                    "every value `{line}` returns becomes {} instead (the returned expressions still run)",
                    self.language.nothing()
                )]
            }
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
                    // Often a call made for what it does, not what it gives
                    // back: saying only "the value becomes nil" hides that.
                    format!(
                        "`{line}` becomes {nothing}: it is not evaluated, so nothing it does happens (no call it makes, no value it stores), and the enclosing block or method produces {nothing}"
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
            Change::Skip => {
                let word = line.trim_end_matches(';').trim();
                vec![match word {
                    "break" => format!(
                        "`{line}` is deleted: execution does not leave the enclosing loop or switch here, and carries on with what follows it"
                    ),
                    "continue" | "next" => format!(
                        "`{line}` is deleted: the rest of the loop body runs in this iteration"
                    ),
                    // A statement of several lines is shown by its first.
                    // Read as "`for x in items:` is deleted", only the header
                    // seemed to go: 22 of 100 such verdicts were wrong on six
                    // projects, 9 with all of it named.
                    _ if whole_statement(s).is_some() => {
                        let lines = s.text.lines().count();
                        format!(
                            "the whole statement starting `{line}` is deleted, all {lines} lines of it (lines {} to {}): none of it runs",
                            s.line,
                            s.line + lines - 1
                        )
                    }
                    // Python: what deleting a return or a raise lets happen.
                    // Of the skipped returns and raises of five projects, 61
                    // of 371 verdicts were wrong as "is deleted" and 44 so.
                    // (TypeScript returns were judged worse this way, 21 of
                    // 82 against 18, and keep the plain wording.)
                    _ if self.language == Language::Python
                        && (word == "return" || word.starts_with("return ")) =>
                    {
                        format!(
                            "`{line}` is deleted: the function does not return here, and execution carries on with whatever follows it"
                        )
                    }
                    _ if self.language == Language::Python
                        && (word == "raise" || word.starts_with("raise ")) =>
                    {
                        format!(
                            "`{line}` is deleted: no error is raised here, and execution carries on with whatever follows it"
                        )
                    }
                    _ => format!("`{line}` is deleted (it never runs)"),
                }]
            }
            Change::ExpressionUndefined => vec![format!(
                "the expression `{line}` evaluates to undefined instead (it is not evaluated)"
            )],
            Change::Flip => vec![format!("`{line}` becomes `{}`", flipped(line))],
            Change::BareError => vec![format!(
                "`{line}` throws `new Error()` instead: execution still stops here, but the error has no message and is not its own type"
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
        let mut state = json!({
            "instructions": INSTRUCTIONS, "runtime": runtime(&t.runner),
            "test": {"file": t.file, "name": t.name},
            "test_code": self.test_code(test),
            "code_run": self.code_run(test, asked)});
        if !self.language.asks_within_the_function() {
            state["helpers"] = json!(self.helpers(test, asked.len()));
        }
        json!({"model": model, "state": state, "questions": questions})
    }

    /// What Jev sees for one statement asked of one test, line numbers aside:
    /// the same code renumbered is the same question. Equal keys, equal question.
    /// Nothing Jev does not see is in it: a key that also held the run's
    /// dependency and configuration fingerprints threw every answer away when
    /// a lockfile or a config file changed, though each question was the same.
    pub fn pair_key(&self, statement: usize, test: usize, model: &str) -> String {
        let t = &self.tests[test];
        let parts = json!([
            model,
            INSTRUCTIONS,
            runtime(&t.runner),
            true,
            &t.file,
            &t.name,
            self.unnumbered_test_codes[test].get_or_init(|| strip_numbers(&self.test_code(test))),
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

/// The line with its boolean literal turned into the other one: the last
/// `true`/`false` (`True`/`False` in Python) that stands as a word.
fn flipped(line: &str) -> String {
    // A trailing comment is not code: `let ready = false // true once open`
    // flipped the comment's `true`.
    let cut = comment_start(line);
    let (code, comment) = line.split_at(cut);
    format!("{}{comment}", flipped_code(code))
}

/// Where a trailing `//` or `# ` comment starts, outside string literals; the
/// line's length when it has none. A bare `#` is code (a private field).
fn comment_start(line: &str) -> usize {
    let bytes = line.as_bytes();
    let mut quote = None;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match quote {
            Some(_) if c == b'\\' => i += 1,
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if matches!(c, b'\'' | b'"' | b'`') => quote = Some(c),
            None if c == b'/' && bytes.get(i + 1) == Some(&b'/') => return i,
            None if c == b'#' && bytes.get(i + 1).is_none_or(|n| *n == b' ') => return i,
            None => {}
        }
        i += 1;
    }
    line.len()
}

fn flipped_code(line: &str) -> String {
    let words = [
        ("true", "false"),
        ("false", "true"),
        ("True", "False"),
        ("False", "True"),
    ];
    let found = words
        .iter()
        .filter_map(|(from, to)| {
            line.match_indices(from)
                .filter(|(at, _)| {
                    let before = line[..*at].chars().next_back();
                    let after = line[at + from.len()..].chars().next();
                    let word = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
                    !word(before) && !word(after)
                })
                .last()
                .map(|(at, _)| (at, *from, *to))
        })
        .max_by_key(|(at, _, _)| *at);
    match found {
        Some((at, from, to)) => format!("{}{to}{}", &line[..at], &line[at + from.len()..]),
        None => line.to_owned(),
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
    let bytes = serde_json::to_vec(files).expect("plain data serializes");
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&bytes).map_err(|e| e.to_string())?;
    let compressed = encoder.finish().map_err(|e| e.to_string())?;
    let temp = path.with_extension("tmp");
    crate::fault_fs::write(&temp, compressed).map_err(|e| e.to_string())?;
    crate::fault_fs::rename(&temp, path).map_err(|e| e.to_string())
}

/// Read files written by [`save_snapshot`].
pub fn load_snapshot(path: &Path) -> Option<BTreeMap<String, String>> {
    use std::io::Read;
    let compressed = crate::fault_fs::read(path).ok()?;
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
    brace_end_within(lines, start, BODY_LINES)
}

/// Where the block opened at `start` closes, however long it is: the region
/// a class, module or group holds, which is searched rather than shown.
fn block_end(lines: &[&str], start: usize) -> usize {
    brace_end_within(lines, start, lines.len())
}

fn brace_end_within(lines: &[&str], start: usize, limit: usize) -> usize {
    let mut depth = 0i64;
    let mut opened = false;
    for (i, line) in lines.iter().enumerate().skip(start).take(limit) {
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
    start
        .saturating_add(limit)
        .min(lines.len())
        .saturating_sub(1)
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

/// The classes a Python test id names around its function: pytest's
/// `file::TestA::TestB::test[param]`, unittest's `module.TestA.test`.
fn python_classes(name: &str) -> Vec<&str> {
    let name = name.split('[').next().unwrap_or(name);
    if name.contains("::") {
        let parts = name.split("::").collect::<Vec<_>>();
        return parts[1..parts.len().saturating_sub(1)].to_vec();
    }
    let parts = name.split('.').collect::<Vec<_>>();
    parts
        .len()
        .checked_sub(2)
        .map(|at| parts[at])
        .filter(|class| class.starts_with(|c: char| c.is_ascii_uppercase()))
        .into_iter()
        .collect()
}

/// The lines of the innermost of `classes`, each found inside the one before
/// and ended by indentation, with the indentation of that class's own body;
/// the whole file when one is not found.
fn python_class_lines(lines: &[&str], classes: &[&str]) -> (usize, usize, Option<usize>) {
    let indent_of = |l: &str| l.len() - l.trim_start().len();
    let (mut from, mut to, mut body) = (0, lines.len(), None);
    for class in classes {
        let declared = (from..to).find(|&i| {
            lines[i]
                .trim_start()
                .strip_prefix("class ")
                .and_then(|rest| rest.strip_prefix(class))
                .is_some_and(|rest| rest.starts_with(['(', ':']))
        });
        let Some(at) = declared else {
            return (0, lines.len(), None);
        };
        let indent = indent_of(lines[at]);
        let end = (at + 1..to)
            .find(|&i| !lines[i].trim().is_empty() && indent_of(lines[i]) <= indent)
            .unwrap_or(to);
        body = (at + 1..end)
            .find(|&i| !lines[i].trim().is_empty())
            .map(|i| indent_of(lines[i]));
        (from, to) = (at, end);
    }
    (from, to, body)
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
            // Every line given starts its number as `{:5} ` or `numbered`
            // writes it: a space always follows.
            if digits > 0 {
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

/// The project-relative name of `path`, `..` and `.` resolved, with `/`
/// between parts on every platform: a run names its files that way, and on
/// Windows a joined path had `\\` and named a helper no run had.
fn normalize(path: &Path) -> String {
    let mut out = Vec::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str().to_string_lossy().into_owned()),
        }
    }
    out.join("/")
}

#[cfg(test)]
mod tests {

    fn questions_of(
        file: &str,
        source: &str,
        lines: &[(usize, &str)],
        language: Language,
    ) -> Vec<Vec<String>> {
        let input = AssessmentInput {
            tests: vec![InputTest {
                id: "t".into(),
                file: Some("t".into()),
                name: "t".into(),
                role: "test".into(),
                runner: "r".into(),
            }],
            points: lines
                .iter()
                .map(|(line, text)| InputPoint {
                    file: file.into(),
                    line: *line,
                    source: (*text).into(),
                    tests: vec![0],
                })
                .collect(),
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([(file.to_owned(), source.to_owned())]);
        let population =
            population_from(Path::new("/nonexistent"), &input, &sources, language).unwrap();
        (0..population.statements.len())
            .map(|s| population.questions(s))
            .collect()
    }

    /// Nothing to assess plans nothing, and a test asked about no statement
    /// is asked nothing: no questions, no code.
    #[test]
    fn a_path_is_named_by_its_parts_with_dots_resolved() {
        assert_eq!(normalize(Path::new("")), "");
        assert_eq!(normalize(Path::new("./src/../lib/a.js")), "lib/a.js");
    }

    #[test]
    fn nothing_to_assess_is_asked_nothing() {
        let empty = AssessmentInput {
            tests: Vec::new(),
            points: Vec::new(),
            decisions: Vec::new(),
        };
        assert!(empty.only_tests(|_| true).tests.is_empty());
        let none = population_from(
            Path::new("/nonexistent"),
            &empty,
            &BTreeMap::new(),
            Language::JavaScript,
        )
        .unwrap();
        assert!(none.statements.is_empty());
        let one = AssessmentInput {
            tests: vec![InputTest {
                id: "t".into(),
                file: Some("t.test.js".into()),
                name: "t".into(),
                role: "test".into(),
                runner: "node:test".into(),
            }],
            points: Vec::new(),
            decisions: Vec::new(),
        };
        let population = population_from(
            Path::new("/nonexistent"),
            &one,
            &BTreeMap::new(),
            Language::JavaScript,
        )
        .unwrap();
        assert!(population.code_run(0, &[]).is_empty());
        assert!(population.files_read().is_empty());
        let request = population.request(0, &[], "m");
        assert_eq!(request["model"], "m");
        assert!(
            request["questions"]
                .as_object()
                .is_none_or(|q| q.is_empty()),
            "{request}"
        );
    }

    /// Each runner a question can name is described, each language read
    /// from its manifest name with its "nothing", and each runner's titles
    /// split where it writes them.
    #[test]
    fn each_runner_and_language_is_named_as_questions_name_it() {
        for (runner, first) in [
            ("node:test", "node:test"),
            ("jest", "Jest"),
            ("ava", "AVA"),
            ("tap", "node-tap"),
            ("mocha", "Mocha"),
            ("pytest", "pytest"),
            ("rspec", "RSpec"),
            ("minitest", "Minitest"),
            ("test-unit", "test-unit"),
            ("cucumber", "Cucumber"),
            ("go-test", "go test"),
            ("rust-libtest", "Rust tests"),
            ("rustdoc", "Rust doctests"),
            ("junit-platform", "JUnit"),
            ("testng", "TestNG"),
            ("unittest", "unittest"),
        ] {
            assert!(
                runtime(runner).starts_with(first),
                "{runner}: {}",
                runtime(runner)
            );
        }
        assert!(!runtime("no-such-runner").is_empty());
        let languages = ["javascript", "python", "ruby", "go", "rust", "jvm"]
            .map(|name| Language::from_manifest(name).unwrap());
        assert_eq!(Language::from_manifest("cobol"), None);
        assert_eq!(
            languages.map(Language::nothing),
            [
                "undefined",
                "None",
                "nil",
                "the zero value",
                "`Default::default()`",
                "the default value (null, or 0 or false for a primitive)",
            ]
        );
        let [javascript, python, ruby, go, rust, jvm] = languages;
        assert_eq!(
            [
                separator(javascript, "jest"),
                separator(python, "pytest"),
                separator(ruby, "rspec"),
                separator(ruby, "minitest"),
                separator(jvm, "junit-platform"),
                separator(go, "go-test"),
                separator(rust, "rustdoc"),
                separator(rust, "rust-libtest"),
            ],
            [" > ", "::", ":", "#", "#", "/", " - ", "::"]
        );
    }

    /// Go, the JVM languages and Rust are asked in their own terms: a zero
    /// value written out, a default value after the expression runs, a
    /// `switch` whose every returned value changes, and an `Ok` that becomes
    /// an error. A replacement that is the statement itself is not asked.
    #[test]
    fn each_language_is_asked_in_its_own_terms() {
        let go = "package a\n\nfunc f(x int) (int, error) {\n\ty := g(x)\n\tif y > 1 {\n\t\treturn y, nil\n\t}\n\treturn 0, nil\n}\n";
        let asked = questions_of(
            "a.go",
            go,
            &[(4, "y := g(x)"), (6, "return y, nil"), (8, "return 0, nil")],
            Language::Go,
        );
        assert_eq!(asked.len(), 2, "{asked:#?}");
        assert!(
            asked[0][0].contains("hold the zero value of their type"),
            "{asked:#?}"
        );
        assert!(
            asked[1][0].contains(
                "becomes `return 0, nil` (the original expression is still evaluated first)"
            ),
            "{asked:#?}"
        );

        let java = "class A {\n  int f(int x) {\n    switch (x) {\n      case 1: return 1;\n      default: return 2;\n    }\n  }\n  int g(int x) {\n    int y = x + 1;\n    return y;\n  }\n}\n";
        let asked = questions_of(
            "A.java",
            java,
            &[
                (
                    3,
                    "switch (x) {\n      case 1: return 1;\n      default: return 2;\n    }",
                ),
                (9, "int y = x + 1;"),
                (10, "return y;"),
            ],
            Language::Jvm,
        );
        assert!(
            asked[0][0].contains("every value `switch (x) {` returns becomes"),
            "{asked:#?}"
        );
        assert!(
            asked[1][0].contains("the variable it assigns holds the default value"),
            "{asked:#?}"
        );
        assert!(
            asked[2][0].contains("returns the default value"),
            "{asked:#?}"
        );

        let rust = "fn f(x: u8) -> Result<u8, ()> {\n    if x > 2 {\n        return Ok(x);\n    }\n    Ok(0)\n}\n";
        let asked = questions_of(
            "src/a.rs",
            rust,
            &[(3, "return Ok(x);"), (5, "Ok(0)")],
            Language::Rust,
        );
        assert!(
            asked[0][0].contains("becomes `return Err(());`"),
            "{asked:#?}"
        );
        assert!(
            asked[1][0].contains("(it is the value the enclosing block produces)"),
            "{asked:#?}"
        );
    }

    #[test]
    fn a_replacement_that_is_the_statement_is_no_change() {
        assert!(super::same_code("return 0, nil", "return 0,  nil"));
        assert!(super::same_code("count := 0", "count := 0;"));
        assert!(!super::same_code("return 0, nil", "return n, nil"));
        let source = "package p\n\nfunc f(n int) (int, error) {\n\tif n > 0 {\n\t\treturn n, nil\n\t}\n\treturn 0, nil\n}\n";
        assert_eq!(
            super::go::rewrite(source, 7, Change::ReturnUndefined).as_deref(),
            Some("return 0, nil")
        );
    }

    use super::*;

    /// Each helper given nothing finds nothing, and one given text it
    /// cannot read hands it back.
    #[test]
    fn helpers_given_nothing_find_nothing() {
        assert!(name_words("").is_empty());
        for text in ["", "'", "''"] {
            assert!(quoted_fragments(text).is_empty(), "{text}");
        }
        assert_eq!(comment_start(""), 0);
        assert_eq!(brace_end_within(&[], 0, 5), 0);
        assert_eq!(brace_end_within(&[""], 0, 1), 0);
        assert!(definitions(&[], Language::Rust).is_empty());
        assert_eq!(split_initializer("x"), None);
        assert_eq!(python_class_lines(&["x = 1"], &["Missing"]), (0, 1, None));
        for line in ["while x {", "if x {", "if (x"] {
            assert_eq!(condition(line), line);
        }
        // Empty and unparsable sources hold no statement and no function.
        for source in ["", "def (:\n"] {
            assert!(python_statement_starts(source).is_empty(), "{source}");
            assert!(
                outermost_functions(Language::Python, "t.py", source).is_empty(),
                "{source}"
            );
        }
        assert!(outermost_functions(Language::JavaScript, "t.js", "").is_empty());
        // An empty namespace and block hold none either.
        assert_eq!(
            outermost_functions(
                Language::JavaScript,
                "t.ts",
                "namespace N {}\n{ }\nfunction f() {}\n"
            )
            .len(),
            1
        );
    }

    #[test]
    fn a_statement_many_tests_run_is_asked_of_the_tests_that_ran_little_else() {
        // Seventeen end-to-end tests pass through `total` and nine other
        // statements; three unit tests, in the file that sorts last, run
        // `total` alone. One test per file in file order never reaches the
        // unit tests within fifteen.
        let source = (0..10)
            .map(|n| format!("const v{n} = compute({n});"))
            .collect::<Vec<_>>()
            .join("\n");
        let test = |file: &str, name: &str| InputTest {
            id: format!("{file}::{name}"),
            file: Some(file.to_owned()),
            name: name.to_owned(),
            role: "test".into(),
            runner: "node:test".into(),
        };
        let mut tests = (0..17)
            .map(|n| {
                let name = if n == 16 {
                    "calls compute on start"
                } else {
                    "serves a request"
                };
                test(&format!("tests/e2e{n:02}.test.js"), name)
            })
            .collect::<Vec<_>>();
        tests.extend((0..3).map(|n| test("tests/zz_unit.test.js", &format!("total case {n}"))));
        let everyone = (0..20).collect::<Vec<u32>>();
        let end_to_end = (0..17).collect::<Vec<u32>>();
        let points = (0..10)
            .map(|n| InputPoint {
                file: "src/sum.js".into(),
                line: n + 1,
                source: format!("const v{n} = compute({n});"),
                tests: if n == 0 {
                    everyone.clone()
                } else {
                    end_to_end.clone()
                },
            })
            .collect();
        let input = AssessmentInput {
            tests,
            points,
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([("src/sum.js".to_owned(), source)]);
        let population = population_from(
            Path::new("/nonexistent"),
            &input,
            &sources,
            Language::JavaScript,
        )
        .unwrap();
        let target = population
            .statements
            .iter()
            .position(|s| s.line == 1)
            .unwrap();
        let unit = |order: &[usize]| {
            order
                .iter()
                .filter(|&&t| population.tests[t].file == "tests/zz_unit.test.js")
                .count()
        };
        let spread = population.order(target, MORE_TESTS);
        assert_eq!(
            unit(&spread),
            0,
            "file order alone leaves the unit tests out"
        );
        let last = population.last_order(target, &spread, LAST_TESTS);
        assert_eq!(last.len(), LAST_TESTS);
        assert!(last.iter().all(|t| !spread.contains(t)), "{last:?}");
        // The test naming what the line calls comes first, though it ran as
        // much as any; then the tests that ran the least.
        assert_eq!(population.tests[last[0]].name, "calls compute on start");
        assert_eq!(unit(&last[1..]), 2, "{last:?}");
        assert_eq!(
            name_words("const hostCount = getHTTPResponse2(send_data)"),
            [
                "const",
                "count",
                "data",
                "host",
                "http",
                "response2",
                "send"
            ]
            .map(String::from)
            .into()
        );
    }

    #[test]
    fn a_statement_is_shown_with_the_function_it_is_in() {
        // The outermost one, with the statement that declares it and what
        // decorates it; a callback written inside it is part of it.
        let typescript = "import { log } from './log'\n\nexport const seal = (key: string) => {\n  const parts = key.split('-').map((part) => {\n    return part.trim()\n  })\n  return parts\n}\n\nclass Vault {\n  size = 0\n  @logged()\n  open(door: string) {\n    return () => door\n  }\n}\n\nserver.on('close', function () {\n  log('closed')\n})\n\nif (flag) {\n  function inner() {\n    return 1\n  }\n}\nconst limit = 4\n";
        assert_eq!(
            outermost_functions(Language::JavaScript, "src/vault.ts", typescript),
            [(3, 8), (12, 15), (18, 20), (23, 25)]
        );
        let python = "import os\n\nLIMIT = 4\n\n@cached\ndef seal(key):\n    def trim(part):\n        return part.strip()\n    return [trim(part) for part in key.split('-')]\n\nclass Vault:\n    size = 0\n\n    async def open(self, door):\n        return lambda: door\n\nif os.name == 'nt':\n    def separator():\n        return ';'\n";
        assert_eq!(
            outermost_functions(Language::Python, "vault.py", python),
            [(5, 9), (14, 15), (18, 19)]
        );
        // The other languages are asked as they were.
        assert!(outermost_functions(Language::Go, "vault.go", "package vault\n").is_empty());
        assert!(Language::Python.asks_within_the_function());
        assert!(!Language::Ruby.asks_within_the_function());
    }

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
        // Measured with its `export`, too.
        assert_eq!(
            change_at(&starts, 2, "export const a = 1", js),
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
    fn a_change_that_cannot_change_anything_is_not_asked() {
        // supergateway's audit: `return undefined` "returning undefined" and
        // `let answered = false` "becoming undefined" (read only as `!answered`)
        // could never be caught, and skipping `super(...)` does not compile.
        let source = "class H extends Map {\n  constructor() {\n    super([])\n  }\n}\nfunction f(a) {\n  if (a) return undefined\n  if (!a) return void 0\n  let answered = false\n  const done = true, more = 1\n  return false\n}\n";
        let starts = statement_starts("x.ts", source);
        let js = Language::JavaScript;
        assert_eq!(change_at(&starts, 3, "super([])", js), None);
        assert_eq!(
            change_at(&starts, 7, "return undefined", js),
            Some(Change::Skip)
        );
        assert_eq!(
            change_at(&starts, 8, "return void 0", js),
            Some(Change::Skip)
        );
        // destr: `return undefined as T` was asked as "returns undefined", which no
        // test can catch, and read as the one statement not asserted.
        let typed = "function f<T>(a: number): T {\n  if (a === 1) return undefined as T\n  if (a === 2) return (undefined as unknown) as T\n  if (a === 3) return <T>undefined\n  if (a === 4) return undefined satisfies unknown\n  return undefined!\n}\n";
        let starts = statement_starts("x.ts", typed);
        for (line, text) in [
            (2, "return undefined as T"),
            (3, "return (undefined as unknown) as T"),
            (4, "return <T>undefined"),
            (5, "return undefined satisfies unknown"),
            (6, "return undefined!"),
        ] {
            assert_eq!(
                change_at(&starts, line, text, js),
                Some(Change::Skip),
                "{text}"
            );
        }
        let starts = statement_starts("x.ts", source);
        assert_eq!(
            change_at(&starts, 9, "let answered = false", js),
            Some(Change::Flip)
        );
        // Two declarators: the value change still applies to both.
        assert_eq!(
            change_at(&starts, 10, "const done = true, more = 1", js),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 11, "return false", js),
            Some(Change::Flip)
        );
        let python = python_statement_starts(
            "def f(a):\n    if a:\n        return None\n    ready = False\n    return True\n",
        );
        let py = Language::Python;
        assert_eq!(change_at(&python, 3, "return None", py), Some(Change::Skip));
        assert_eq!(
            change_at(&python, 4, "ready = False", py),
            Some(Change::Flip)
        );
        assert_eq!(change_at(&python, 5, "return True", py), Some(Change::Flip));
        assert_eq!(flipped("let answered = false"), "let answered = true");
        assert_eq!(flipped("return isTrue && false;"), "return isTrue && true;");
        assert_eq!(flipped("ready = False"), "ready = True");
        assert_eq!(
            flipped("let ready = false // true once open"),
            "let ready = true // true once open"
        );
        assert_eq!(
            flipped("ready = False  # True later"),
            "ready = True  # True later"
        );
        assert_eq!(flipped("this.#open = false"), "this.#open = true");
        assert_eq!(
            flipped("const s = 'a // b', on = false"),
            "const s = 'a // b', on = true"
        );
    }

    #[test]
    fn a_typescript_throw_keeps_its_types() {
        // supergateway's modernHttp.ts:294: deleting the guard's throw made
        // `result.result` fail to type-check on the next line (TS2339).
        let source = "async function list(reply) {\n  const result = await reply\n  if ('error' in result)\n    throw new Error(`tools/list failed: ${result.error.message}`)\n  return result.result\n}\n";
        let typed = statement_starts("x.ts", source);
        let js = Language::JavaScript;
        assert_eq!(
            change_at(
                &typed,
                4,
                "throw new Error(`tools/list failed: ${result.error.message}`)",
                js
            ),
            Some(Change::BareError)
        );
        let plain = statement_starts("x.js", source);
        assert_eq!(
            change_at(
                &plain,
                4,
                "throw new Error(`tools/list failed: ${result.error.message}`)",
                js
            ),
            Some(Change::Skip)
        );
    }

    #[test]
    fn a_deleted_statement_says_what_goes_and_what_then_runs() {
        let source = "def f(items, n):\n    if n < 0:\n        raise ValueError(\"negative\")\n    if not items:\n        return None\n    for item in items:\n        use(item)\n    log(n)\n    return n\n";
        let point = |line: usize, text: &str| InputPoint {
            file: "pkg/f.py".into(),
            line,
            source: text.into(),
            tests: vec![0],
        };
        let input = AssessmentInput {
            tests: vec![InputTest {
                id: "tests/test_f.py::test_f".into(),
                file: Some("tests/test_f.py".into()),
                name: "tests/test_f.py::test_f".into(),
                role: "test".into(),
                runner: "pytest".into(),
            }],
            points: vec![
                point(3, "raise ValueError(\"negative\")"),
                point(5, "return None"),
                point(6, "for item in items:\n        use(item)"),
                point(8, "log(n)"),
            ],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([("pkg/f.py".to_owned(), source.to_owned())]);
        let python = population_from(
            Path::new("/nonexistent"),
            &input,
            &sources,
            Language::Python,
        )
        .unwrap();
        let question = |population: &Population, line: usize| {
            let at = population
                .statements
                .iter()
                .position(|s| s.line == line)
                .unwrap();
            population.questions(at).join(" | ")
        };
        assert_eq!(
            question(&python, 3),
            "pkg/f.py:3: `raise ValueError(\"negative\")` is deleted: no error is raised here, and execution carries on with whatever follows it."
        );
        assert_eq!(
            question(&python, 5),
            "pkg/f.py:5: `return None` is deleted: the function does not return here, and execution carries on with whatever follows it."
        );
        assert_eq!(
            question(&python, 6),
            "pkg/f.py:6: the whole statement starting `for item in items:` is deleted, all 2 lines of it (lines 6 to 7): none of it runs."
        );
        assert_eq!(
            question(&python, 8),
            "pkg/f.py:8: `log(n)` is deleted (it never runs)."
        );
        // A JavaScript return keeps the plain wording: it was judged worse
        // the other way.
        let js = AssessmentInput {
            tests: vec![InputTest {
                id: "t".into(),
                file: Some("test/g.test.js".into()),
                name: "g".into(),
                role: "test".into(),
                runner: "node:test".into(),
            }],
            points: vec![InputPoint {
                file: "src/g.js".into(),
                line: 3,
                source: "return;".into(),
                tests: vec![0],
            }],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([(
            "src/g.js".to_owned(),
            "function g(a) {\n  if (a) {\n    return;\n  }\n  use(a);\n}\n".to_owned(),
        )]);
        let javascript = population_from(
            Path::new("/nonexistent"),
            &js,
            &sources,
            Language::JavaScript,
        )
        .unwrap();
        assert_eq!(
            question(&javascript, 3),
            "src/g.js:3: `return;` is deleted (it never runs)."
        );
    }

    #[test]
    fn a_statement_s_text_is_what_a_test_would_quote() {
        assert_eq!(
            quoted_fragments("logger.info(`  - Headers: ${describeHeaders(headers)}`)"),
            vec!["- Headers:"]
        );
        assert_eq!(
            quoted_fragments("logger.error(`Child exited: code=${code}, signal=${signal}`)"),
            vec![", signal=", "Child exited: code="]
        );
        assert_eq!(
            quoted_fragments("log(f\"Serving {path} on {port}\")"),
            vec!["Serving"]
        );
        // Short or letterless pieces say nothing about a test.
        assert!(quoted_fragments("x = 'abc' + \"1234567\"").is_empty());
        assert!(quoted_fragments("return a + b").is_empty());
    }

    /// The code shown for a test, in a fresh directory holding its file.
    fn shown_test_code(
        file: &str,
        text: &str,
        name: &str,
        runner: &str,
        language: Language,
    ) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-shown-test-code-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        let input = AssessmentInput {
            tests: vec![InputTest {
                id: name.into(),
                file: Some(file.into()),
                name: name.into(),
                role: "test".into(),
                runner: runner.into(),
            }],
            points: vec![InputPoint {
                file: "src/a".into(),
                line: 1,
                source: "x".into(),
                tests: vec![0],
            }],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([("src/a".to_owned(), "x\n".to_owned())]);
        population_from(&root, &input, &sources, language)
            .unwrap()
            .test_code(0)
    }

    /// A Ruby test's helpers are what it loads that is not measured source,
    /// cut to fit and stopping once full; the support files its constants
    /// name follow, cut to their own allowance and stopping once it is spent.
    #[test]
    fn a_ruby_test_s_helpers_and_support_files_are_shown_within_their_limits() {
        let root = std::env::temp_dir().join(format!(
            "supercov-ruby-helpers-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        let write = |file: &str, text: &str| {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        };
        write("lib/m.rb", "def m\n  1\nend\n");
        write("spec/spec_helper.rb", &"# helper\n".repeat(500));
        write("spec/support/fake_api.rb", "class FakeApi\nend\n");
        write("spec/support/big_fixture.rb", &"# fixture\n".repeat(2000));
        write(
            "spec/a_spec.rb",
            "require_relative '../lib/m'\nrequire_relative 'support/fake_api'\nRSpec.describe FakeApi do\n  it { BigFixture }\nend\n",
        );
        let input = AssessmentInput {
            tests: vec![InputTest {
                id: "spec/a_spec.rb[1:1]".into(),
                file: Some("spec/a_spec.rb".into()),
                name: "spec/a_spec.rb[1:1]".into(),
                role: "test".into(),
                runner: "rspec".into(),
            }],
            points: vec![InputPoint {
                file: "lib/m.rb".into(),
                line: 2,
                source: "1".into(),
                tests: vec![0],
            }],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([("lib/m.rb".to_owned(), "def m\n  1\nend\n".to_owned())]);
        let population = population_from(&root, &input, &sources, Language::Ruby).unwrap();
        let helpers = population.helpers(0, 1);
        assert!(
            !helpers.contains_key("lib/m.rb"),
            "measured source is not a helper"
        );
        let helper = &helpers["spec/spec_helper.rb"];
        assert!(helper.contains("... (cut)"), "{helper}");
        assert!(helper.len() < 4000, "{}", helper.len());
        let fixture = &helpers["spec/support/big_fixture.rb"];
        assert!(fixture.contains("... (cut)"));
        // The helper filled its allowance before the require came up, and
        // the fixture filled the support files' before this one did.
        assert!(
            !helpers.contains_key("spec/support/fake_api.rb"),
            "{:?}",
            helpers.keys()
        );
    }

    /// A JVM test's code leaves out block comments -- a license header, a
    /// one-line `/* */`, one inside the body -- and keeps the code.
    #[test]
    fn block_comments_are_left_out_of_a_jvm_test_s_code() {
        let text = "/*\n * License header\n */\npackage shop;\n\nimport org.junit.jupiter.api.Test;\n/* one-line */\nclass CartTest {\n    @Test\n    void adds() {\n        /* inside\n           block */\n        assertEquals(2, add(1, 1));\n    }\n}\n";
        let code = shown_test_code(
            "src/test/java/shop/CartTest.java",
            text,
            "shop.CartTest#adds()",
            "junit",
            Language::Jvm,
        );
        for kept in [
            "package shop;",
            "import org.junit",
            "void adds()",
            "assertEquals(2, add(1, 1));",
        ] {
            assert!(code.contains(kept), "{kept}: {code}");
        }
        for left in ["License header", "one-line", "inside", "block */"] {
            assert!(!code.contains(left), "{left}: {code}");
        }
        // A method the file does not have: its head, then the rest of it.
        let missing = shown_test_code(
            "src/test/java/shop/CartTest.java",
            text,
            "shop.CartTest#gone()",
            "junit",
            Language::Jvm,
        );
        assert!(missing.contains("  ..."), "{missing}");
        assert!(missing.contains("assertEquals"), "{missing}");
    }

    /// A line that opens with a block comment closed before its code is
    /// code: the statement on it is shown, marked as run, and a test's line
    /// written that way is kept.
    #[test]
    fn a_statement_after_a_block_comment_on_its_line_is_shown() {
        let source = "export function limit(read) {\n  /** @type {number} */ const max = read();\n  return max;\n}\n";
        let input = AssessmentInput {
            tests: vec![InputTest {
                id: "t".into(),
                file: Some("test/limit.test.js".into()),
                name: "limit".into(),
                role: "test".into(),
                runner: "node:test".into(),
            }],
            points: vec![InputPoint {
                file: "src/limit.js".into(),
                line: 2,
                source: "const max = read();".into(),
                tests: vec![0],
            }],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([("src/limit.js".to_owned(), source.to_owned())]);
        let population = population_from(
            Path::new("/nonexistent"),
            &input,
            &sources,
            Language::JavaScript,
        )
        .unwrap()
        .with_files(sources);
        let code = &population.code_run(0, &[0])["src/limit.js"];
        assert!(
            code.contains("▶    2   /** @type {number} */ const max = read();"),
            "{code}"
        );
        assert!(!code.contains("1 lines not shown\n     3"), "{code}");
        let java = "package shop;\n\nclass CartTest {\n    @Test\n    void adds() {\n        /* given */ cart.add(1);\n        /* then */\n        assertEquals(1, cart.size());\n    }\n}\n";
        let code = shown_test_code(
            "src/test/java/shop/CartTest.java",
            java,
            "shop.CartTest#adds()",
            "junit",
            Language::Jvm,
        );
        assert!(code.contains("/* given */ cart.add(1);"), "{code}");
        assert!(!code.contains("/* then */"), "{code}");
        for (line, comment) in [
            ("/* a */ run();", false),
            ("/**/", true),
            ("/* a */ // b", true),
            ("/* open", true),
        ] {
            assert_eq!(Language::JavaScript.comment(line), comment, "{line}");
        }
    }

    /// An `async def` test is found; one not in the file shows the file.
    #[test]
    fn a_python_test_is_found_by_async_def_or_shown_as_its_file() {
        let constants = (0..12).map(|i| format!("C{i} = {i}\n")).collect::<String>();
        let text = &format!(
            "import pytest\n{constants}\n\n@pytest.mark.asyncio\nasync def test_fetch():\n    assert await fetch() == 1\n\n\ndef test_other():\n    assert True\n"
        );
        let code = shown_test_code(
            "tests/test_fetch.py",
            text,
            "tests/test_fetch.py::test_fetch",
            "pytest",
            Language::Python,
        );
        assert!(code.contains("@pytest.mark.asyncio"), "{code}");
        assert!(code.contains("assert await fetch() == 1"), "{code}");
        assert!(!code.contains("test_other"), "{code}");
        let missing = shown_test_code(
            "tests/test_fetch.py",
            text,
            "tests/test_fetch.py::test_gone",
            "pytest",
            Language::Python,
        );
        assert!(missing.contains("  ..."), "{missing}");
    }

    /// A pytest method far into its file: the file's first lines, then the
    /// class around it, its decorators and its body, each once.
    #[test]
    fn a_python_test_deep_in_its_file_is_shown_with_its_class_and_decorators() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-python-test-code-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(root.join("tests")).unwrap();
        let mut file =
            String::from("# The shop's tests.\nimport pytest\n\nfrom shop import total\n");
        for i in 0..10 {
            file.push_str(&format!("\n\ndef helper_{i}():\n    return {i}\n"));
        }
        file.push_str("\n\nclass TestTotal:\n    def test_first(self):\n        assert total([]) == 0\n\n    @pytest.mark.slow\n    @pytest.mark.parametrize(\"n\", [1])\n    def test_deep(self, n):\n        # one item\n        assert total([n]) == n\n\n    def test_after(self):\n        pass\n");
        std::fs::write(root.join("tests/test_total.py"), &file).unwrap();
        let input = AssessmentInput {
            tests: vec![InputTest {
                id: "tests/test_total.py::TestTotal::test_deep[1]".into(),
                file: Some("tests/test_total.py".into()),
                name: "tests/test_total.py::TestTotal::test_deep[1]".into(),
                role: "test".into(),
                runner: "pytest".into(),
            }],
            points: vec![InputPoint {
                file: "shop.py".into(),
                line: 1,
                source: "return 0".into(),
                tests: vec![0],
            }],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([("shop.py".to_owned(), "return 0\n".to_owned())]);
        let population = population_from(&root, &input, &sources, Language::Python).unwrap();
        let code = population.test_code(0);
        std::fs::remove_dir_all(&root).unwrap();
        let shown = code
            .lines()
            .map(|line| {
                let line = line.trim_start();
                line.split_once(' ').map_or(line, |(_, l)| l.trim_start())
            })
            .collect::<Vec<_>>();
        assert_eq!(
            shown,
            [
                "import pytest",
                "",
                "from shop import total",
                "",
                "",
                "def helper_0():",
                "return 0",
                "",
                "",
                "...",
                "class TestTotal:",
                "...",
                "@pytest.mark.slow",
                "@pytest.mark.parametrize(\"n\", [1])",
                "def test_deep(self, n):",
                "assert total([n]) == n",
                "",
            ],
            "{code}"
        );
    }

    /// The small readers a question is built with: a statement's condition,
    /// the part of a line before its value, a test call, and the numbers and
    /// locations a cache key leaves out.
    #[test]
    fn statement_text_is_read_the_way_each_language_writes_it() {
        assert_eq!(condition("if (a && b) {"), "a && b");
        assert_eq!(condition("} else if (ready) {"), "ready");
        assert_eq!(condition("if (a) return;"), "if (a) return;");
        assert_eq!(condition("while (a) {"), "while (a) {");
        assert_eq!(python_condition("elif x is None:"), "x is None");
        assert_eq!(python_condition("if ready:"), "ready");
        assert_eq!(
            split_initializer("const x = f(1);").as_deref(),
            Some("const x =")
        );
        assert_eq!(
            split_initializer("if (a <= b && c != d) total = 1").as_deref(),
            Some("if (a <= b && c != d) total =")
        );
        assert_eq!(split_initializer("a === b"), None);
        assert_eq!(split_initializer("items.map((x) => x)"), None);
        assert!(is_test_call("  it('works', () => {"));
        assert!(is_test_call("test.only('works', () => {"));
        assert!(is_test_call("test ('spaced', () => {"));
        assert!(!is_test_call("items.push(1)"));
        assert!(!is_test_call("testing(1)"));
        assert_eq!(
            strip_numbers("▶   12 total(1)\n    13 next\n  ... 4 lines not shown\nplain"),
            "▶total(1)\n next\n...\nplain"
        );
        assert_eq!(
            strip_location("src/a.js:12: `x` is deleted"),
            "src/a.js:: `x` is deleted"
        );
        assert_eq!(strip_location("src/a.js:main: kept"), "src/a.js:main: kept");
        assert_eq!(strip_location("no location"), "no location");
    }

    /// A helper file's named definitions, so a test is sent only the helpers
    /// it names: methods with their bodies (a brace on its own line too),
    /// Kotlin functions, RSpec `let` and `subject`, Rust items.
    #[test]
    fn helper_definitions_are_read_the_way_each_language_declares_them() {
        let names = |language: Language, text: &str| {
            let lines = text.split('\n').collect::<Vec<_>>();
            definitions(&lines, language)
                .into_iter()
                .map(|(name, start, end)| format!("{name} {}-{}", start + 1, end + 1))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(
                Language::Jvm,
                "class Helpers {\n  static int total(int a) {\n    return a;\n  }\n  private static final Map<String, Integer> PRICES = Map.of(\"a\", 1);\n  public <T> List<T> wrap(T value)\n  {\n    return List.of(value);\n  }\n  void call() { helper(1); }\n}\n"
            ),
            ["total 2-4", "wrap 6-9", "call 10-10"]
        );
        assert_eq!(
            names(
                Language::Jvm,
                "object Fixtures {\n    private fun price(x: Int) = x * 2\n    val list = listOf(1).map { it }\n    suspend fun later() {\n    }\n}\n"
            ),
            ["price 2-2", "later 4-5"]
        );
        assert_eq!(
            names(
                Language::Ruby,
                "module Helpers\n  def self.make(x)\n    x\n  end\n  let(:order) { 1 }\n  let!(:user) { 2 }\n  subject(:cart) { 3 }\n  subject { 4 }\n  def valid?\n    true\n  end\nend\n"
            ),
            [
                "make 2-4",
                "order 5-5",
                "user 6-6",
                "cart 7-7",
                "subject 8-8",
                "valid? 9-11"
            ]
        );
        assert_eq!(
            names(
                Language::Rust,
                "pub fn make() -> u8 {\n    1\n}\npub(crate) const LIMIT: u8 = 2;\nstruct Point {\n    x: u8,\n}\n"
            ),
            ["make 1-3", "LIMIT 4-4", "Point 5-7"]
        );
    }

    #[test]
    fn a_python_test_is_found_in_the_class_its_id_names() {
        let text = "import pytest\n\n\nclass TestCart:\n    def test_total(self):\n        assert cart() == 0\n\n\nclass TestOrder:\n    class TestRefund:\n        def test_total(self):\n            assert refund() == 1\n\n    def test_total(self):\n        assert order() == 2\n";
        let lines = text.split('\n').collect::<Vec<_>>();
        let at = |name: &str| {
            let (from, to, body) = python_class_lines(&lines, &python_classes(name));
            (from..to)
                .find(|&i| {
                    lines[i].trim_start().starts_with("def test_total(")
                        && body
                            .is_none_or(|body| lines[i].len() - lines[i].trim_start().len() == body)
                })
                .map(|i| i + 1)
        };
        assert_eq!(at("tests/test_a.py::TestCart::test_total"), Some(5));
        assert_eq!(
            at("tests/test_a.py::TestOrder::TestRefund::test_total[1]"),
            Some(11)
        );
        // The class's own method, not the one of a class nested in it.
        assert_eq!(at("tests/test_a.py::TestOrder::test_total"), Some(14));
        assert_eq!(at("tests.test_a.TestCart.test_total"), Some(5));
        assert_eq!(
            python_classes("tests/test_a.py::test_plain"),
            Vec::<&str>::new()
        );
        assert_eq!(
            python_classes("tests.test_a.test_plain"),
            Vec::<&str>::new()
        );
    }

    /// A boolean assigned or returned is flipped; one of several targets,
    /// or a value that is not a literal, becomes None instead.
    #[test]
    fn python_booleans_flip_and_other_values_become_none() {
        let source = "def f(flag):\n    a = b = True\n    x: bool = True\n    y: bool = flag\n    return True\n    return None\n";
        let starts = python_statement_starts(source);
        let py = Language::Python;
        assert_eq!(
            change_at(&starts, 2, "a = b = True", py),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 3, "x: bool = True", py),
            Some(Change::Flip)
        );
        assert_eq!(
            change_at(&starts, 4, "y: bool = flag", py),
            Some(Change::ValueUndefined)
        );
        assert_eq!(change_at(&starts, 5, "return True", py), Some(Change::Flip));
        assert_eq!(change_at(&starts, 6, "return None", py), Some(Change::Skip));
        // No statement there: JavaScript reads the expression as undefined,
        // other languages do not guess.
        let none = BTreeMap::new();
        assert_eq!(
            change_at(&none, 1, "go()", Language::JavaScript),
            Some(Change::ExpressionUndefined)
        );
        assert_eq!(change_at(&none, 1, "go()", Language::Rust), None);
    }

    /// A title not quoted is found where its words are, inside its group
    /// first and then anywhere; an empty title is not looked for.
    #[test]
    fn a_javascript_test_title_falls_back_to_its_words() {
        let lines = ["describe('g', () => {", "  test(x)", "});", "y = 1"];
        assert_eq!(js_test_line(&lines, "g > x"), Some(1));
        assert_eq!(js_test_line(&lines, "zzz > y"), Some(3));
        assert_eq!(js_test_line(&lines, " > y"), Some(3));
        assert_eq!(js_test_line(&lines, ""), None);
        assert_eq!(js_test_line(&lines, "g > nowhere"), None);
    }

    #[test]
    fn a_javascript_test_is_found_in_the_group_its_name_gives() {
        let text = "import { totals } from '../src/cart.js';\n\ndescribe('cart', () => {\n  it('totals', () => {\n    expect(totals([])).toBe(0);\n  });\n});\n\ndescribe('order', () => {\n  describe('refund', () => {\n    it('totals', () => {});\n  });\n  it('totals', () => {\n    expect(1).toBe(1);\n  });\n});\n";
        let lines = text.split('\n').collect::<Vec<_>>();
        let line = |name: &str| js_test_line(&lines, name).map(|i| i + 1);
        assert_eq!(
            line("cart > totals"),
            Some(4),
            "not the import that names it"
        );
        assert_eq!(line("order > refund > totals"), Some(11));
        assert_eq!(line("totals"), Some(4));
        // A group longer than a body is shown is still searched to its end.
        let long = format!(
            "describe('a', () => {{\n  it('x', () => {{}});\n}});\ndescribe('b', () => {{\n{}  it('x', () => {{}});\n}});\n",
            "  const y = 1;\n".repeat(200)
        );
        let lines = long.split('\n').collect::<Vec<_>>();
        assert_eq!(js_test_line(&lines, "b > x").map(|i| i + 1), Some(205));
    }

    /// Module-level code runs once, for whichever test loaded the module:
    /// every test that runs the module's functions relies on it too.
    #[test]
    fn a_module_level_statement_is_credited_to_every_test_that_uses_the_module() {
        let source =
            "const limit = 10;\nexport function total(n) {\n  return n > limit ? limit : n;\n}\n";
        let test = |name: &str| InputTest {
            id: format!("t/{name}"),
            file: Some("test/total.test.js".into()),
            name: name.into(),
            role: "test".into(),
            runner: "node:test".into(),
        };
        let input = AssessmentInput {
            tests: vec![
                test("rejects without a key"),
                test("caps"),
                test("passes small"),
            ],
            points: vec![
                InputPoint {
                    file: "src/total.js".into(),
                    line: 1,
                    source: "const limit = 10;".into(),
                    tests: vec![0],
                },
                InputPoint {
                    file: "src/total.js".into(),
                    line: 3,
                    source: "return n > limit ? limit : n;".into(),
                    tests: vec![1, 2],
                },
            ],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([("src/total.js".to_owned(), source.to_owned())]);
        let population = population_from(
            Path::new("/nonexistent"),
            &input,
            &sources,
            Language::JavaScript,
        )
        .unwrap();
        let tests_of = |line: usize| {
            let mut tests = population
                .statements
                .iter()
                .find(|s| s.line == line)
                .unwrap()
                .tests
                .clone();
            tests.sort_unstable();
            tests
        };
        assert_eq!(tests_of(1), [0, 1, 2], "the loader and the module's users");
        assert_eq!(
            tests_of(3),
            [1, 2],
            "code in a function keeps its own tests"
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

    /// A fresh directory holding these files.
    fn project(files: &[(&str, &str)]) -> crate::test_directory::TestDirectory {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-assertion-project-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        for (file, text) in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    fn input_test(file: Option<&str>, name: &str, runner: &str) -> InputTest {
        InputTest {
            id: name.into(),
            file: file.map(Into::into),
            name: name.into(),
            role: "test".into(),
            runner: runner.into(),
        }
    }

    fn point(file: &str, line: usize, source: &str, tests: &[u32]) -> InputPoint {
        InputPoint {
            file: file.into(),
            line,
            source: source.into(),
            tests: tests.to_vec(),
        }
    }

    /// An assessment asks about the statements the run measured and the
    /// decisions it executed: not a statement it declined, a function, or a
    /// decision no test reached.
    #[test]
    fn an_assessment_reads_measured_statements_and_executed_decisions() {
        use crate::coverage_analysis::McdcVector;
        use crate::coverage_report::{
            CoverageManifest, CoverageReportRequest, DecisionMeta, DecisionSnapshot, ExitCodeInput,
            PointMeta, RawTestResult, RuntimeSnapshot, TestProvenance, analyze_coverage_results,
        };
        let decision = |id: &str, line: usize| DecisionMeta {
            id: id.into(),
            file: "src/a.js".into(),
            line,
            column: 3,
            source: "ready".into(),
            conditions: vec!["ready".into()],
            kind: "if".into(),
        };
        let point = |id: &str, kind: PointKind, line: usize, source: &str| PointMeta {
            id: id.into(),
            kind,
            file: "src/a.js".into(),
            line,
            column: 3,
            source: source.into(),
            label: None,
        };
        let report = analyze_coverage_results(&CoverageReportRequest {
            run_id: "run".into(),
            manifest: CoverageManifest {
                unmeasured: vec!["declined".into()],
                decisions: vec![decision("ran", 2), decision("skipped", 5)],
                points: vec![
                    point("function", PointKind::Function, 1, "function f() {"),
                    point("work", PointKind::Statement, 3, "work();"),
                    point("declined", PointKind::Statement, 4, "eval(code);"),
                ],
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
                assertion_sites: Vec::new(),
            },
            raw_results: vec![RawTestResult {
                span: None,
                test_id: Some("test".into()),
                scope: None,
                test: "test".into(),
                test_file: Some("tests/a.js".into()),
                title: None,
                retry: Some(0),
                status: Some("passed".into()),
                expected_status: None,
                flaky: false,
                provenance: TestProvenance {
                    runner: "node:test".into(),
                    kind: "unit".into(),
                    project: None,
                    source: "runner-default".into(),
                    tags: Vec::new(),
                },
                role: "test".into(),
                attribution: crate::coverage_report::ATTRIBUTION_EXACT.into(),
                phases: Vec::new(),
                runtime: vec![RuntimeSnapshot {
                    decisions: vec![DecisionSnapshot {
                        meta: decision("ran", 2),
                        vectors: vec![McdcVector {
                            values: vec![Some(true)],
                            outcome: true,
                        }],
                    }],
                    hits: vec!["function".into(), "work".into(), "declined".into()],
                    events: Vec::new(),
                    logicals: Vec::new(),
                    phase_id: None,
                }],
                browser: Vec::new(),
                server: Vec::new(),
            }],
            generated_at: "time".into(),
            coverage_model: None,
            integrity: None,
            test_exit_code: ExitCodeInput::Present(Some(0)),
        })
        .unwrap();
        let input = assessment_input(&report);
        assert_eq!(input.points, [self::point("src/a.js", 3, "work();", &[0])]);
        assert_eq!(input.decisions, [self::point("src/a.js", 2, "ready", &[0])]);
        assert_eq!(
            input.tests,
            [InputTest {
                id: "test".into(),
                file: Some("tests/a.js".into()),
                name: "test".into(),
                role: "test".into(),
                runner: "node:test".into(),
            }]
        );
    }

    /// A JVM test is named by its class, and found by the path that class
    /// would have when the run gives it no file; elsewhere such a test is
    /// not asked, nor is one whose class has no file.
    #[test]
    fn a_test_without_a_file_is_found_by_its_class_on_the_jvm_alone() {
        let root = project(&[
            (
                "src/test/java/shop/CartTest.java",
                "package shop;\nclass CartTest {}\n",
            ),
            ("AcmeCartTest.java", "class AcmeCartTest {}\n"),
            ("CartTest.java", "class CartTest {}\n"),
        ]);
        let cart =
            "package shop;\n\nclass Cart {\n  int total(int a) {\n    return a + 1;\n  }\n}\n";
        let input = AssessmentInput {
            tests: vec![
                input_test(None, "shop.CartTest#adds()", "junit-platform"),
                input_test(None, "CartTest#adds()", "junit-platform"),
                input_test(None, "gone.Missing#adds()", "junit-platform"),
            ],
            points: vec![point(
                "src/main/java/shop/Cart.java",
                5,
                "return a + 1;",
                &[0, 1, 2],
            )],
            decisions: Vec::new(),
        };
        let sources =
            BTreeMap::from([("src/main/java/shop/Cart.java".to_owned(), cart.to_owned())]);
        let jvm = population_from(&root, &input, &sources, Language::Jvm).unwrap();
        assert_eq!(
            jvm.tests
                .iter()
                .map(|t| t.file.as_str())
                .collect::<Vec<_>>(),
            ["src/test/java/shop/CartTest.java", "CartTest.java"]
        );
        assert_eq!(jvm.statements[0].tests, [0, 1]);
        let python = population_from(&root, &input, &sources, Language::Python).unwrap();
        assert!(python.tests.is_empty());
        assert!(python.statements.is_empty());
    }

    /// A Rust statement in a test, bench or example file is the test's own
    /// code, not code under test; and a decision is asked only as the
    /// condition it is, not as a statement that starts like it.
    #[test]
    fn test_code_and_decisions_that_are_no_condition_are_not_asked() {
        let input = AssessmentInput {
            tests: vec![input_test(
                Some("tests/it.rs"),
                "tests/it.rs::adds",
                "rust-libtest",
            )],
            points: vec![
                point("src/lib.rs", 2, "let y = x + 1;", &[0]),
                point("tests/it.rs", 2, "let z = 2;", &[0]),
            ],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([
            (
                "src/lib.rs".to_owned(),
                "pub fn f(x: u8) -> u8 {\n    let y = x + 1;\n    y\n}\n".to_owned(),
            ),
            (
                "tests/it.rs".to_owned(),
                "fn helper() -> u8 {\n    let z = 2;\n    z\n}\n".to_owned(),
            ),
        ]);
        let rust =
            population_from(Path::new("/nonexistent"), &input, &sources, Language::Rust).unwrap();
        assert_eq!(
            rust.statements
                .iter()
                .map(|s| s.file.as_str())
                .collect::<Vec<_>>(),
            ["src/lib.rs"]
        );
        // `queue` is the loop's condition, though `queue.pop()` starts with it.
        let source = "def f(v, queue):\n    if v:\n        pass\n    elif v is None:\n        pass\n    while queue: queue.pop()\n";
        let input = AssessmentInput {
            tests: vec![input_test(Some("tests/test_f.py"), "test_f", "pytest")],
            points: Vec::new(),
            decisions: vec![
                point("f.py", 4, "v is None", &[0]),
                point("f.py", 6, "queue", &[0]),
            ],
        };
        let sources = BTreeMap::from([("f.py".to_owned(), source.to_owned())]);
        let python = population_from(
            Path::new("/nonexistent"),
            &input,
            &sources,
            Language::Python,
        )
        .unwrap();
        assert_eq!(
            python
                .statements
                .iter()
                .map(|s| (s.line, s.change))
                .collect::<Vec<_>>(),
            [(4, Change::Invert)]
        );
    }

    /// An accessor field and a static block are each one unit of their
    /// class, as a method and a property are, whatever they hold.
    #[test]
    fn an_accessor_and_a_static_block_are_each_a_unit_of_their_class() {
        let source = "class A {\n  accessor handler = () => {\n    return 1\n  };\n  static {\n    function setup() {\n      return 2\n    }\n  }\n}\n";
        assert_eq!(
            outermost_functions(Language::JavaScript, "src/a.js", source),
            [(2, 4), (5, 9)]
        );
    }

    /// An overload signature, or a function only declared, has no body to
    /// run: only the implementation is a function a statement is in.
    #[test]
    fn a_typescript_signature_without_a_body_is_no_function() {
        let source = "export function f(a: string): string;\nexport function f(a: number): number;\nexport function f(a: any) {\n  return a\n}\ndeclare function g(): void;\n";
        assert_eq!(
            outermost_functions(Language::JavaScript, "src/f.ts", source),
            [(3, 5)]
        );
    }

    /// Tests whose file quotes what a statement writes are asked first, one
    /// per file, then the rest of them; the groups of every file follow,
    /// round-robin, each test asked once however it was picked.
    #[test]
    fn tests_that_quote_a_statement_are_asked_first_and_once() {
        let quotes = "expect(log).toContain(\"Server listening on port\");\n";
        let tests = [
            ("tests/a.test.js", "x > one"),
            ("tests/a.test.js", "y > two"),
            ("tests/b.test.js", "x > three"),
            ("tests/c.test.js", "z > four"),
            ("tests/c.test.js", "w > seven"),
            ("tests/c.test.js", "w > eight"),
            ("tests/d.test.js", "x > five"),
            ("tests/d.test.js", "x > six"),
        ];
        let input = AssessmentInput {
            tests: tests
                .iter()
                .map(|(file, name)| input_test(Some(file), name, "vitest"))
                .collect(),
            points: vec![point(
                "src/server.js",
                1,
                "log(\"Server listening on port \" + port);",
                &(0..8).collect::<Vec<_>>(),
            )],
            decisions: Vec::new(),
        };
        let mut files = BTreeMap::from([(
            "src/server.js".to_owned(),
            "log(\"Server listening on port \" + port);\n".to_owned(),
        )]);
        let population = population_from(
            Path::new("/nonexistent"),
            &input,
            &files,
            Language::JavaScript,
        )
        .unwrap();
        for file in ["tests/a.test.js", "tests/b.test.js", "tests/d.test.js"] {
            files.insert(file.into(), quotes.into());
        }
        files.insert("tests/c.test.js".into(), "expect(1).toBe(1);\n".into());
        let population = population.with_files(files);
        let names = |picked: Vec<usize>| {
            picked
                .into_iter()
                .map(|t| {
                    population.tests[t]
                        .name
                        .rsplit(' ')
                        .next()
                        .unwrap()
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(names(population.order(0, 3)), ["one", "three", "five"]);
        assert_eq!(
            names(population.order(0, 7)),
            ["one", "three", "five", "two", "six", "eight", "four"]
        );
    }

    /// A JavaScript test's body runs to the next test at its own depth: a
    /// call deeper in that reads like a test is part of it, and its comment
    /// lines are left out.
    #[test]
    fn a_javascript_test_ends_at_the_next_test_beside_it() {
        let fixtures = (0..10)
            .map(|n| format!("const fixture{n} = {n};\n"))
            .collect::<String>();
        let text = format!(
            "{fixtures}describe('cart', () => {{\n  it('totals', () => {{\n    // the empty cart\n    expect(total([])).toBe(0);\n      it.todo('rounds');\n    expect(total([1])).toBe(1);\n  }});\n  it('next', () => {{\n    expect(2).toBe(2);\n  }});\n}});\n"
        );
        let code = shown_test_code(
            "test/cart.test.js",
            &text,
            "cart > totals",
            "vitest",
            Language::JavaScript,
        );
        assert!(code.contains("it.todo('rounds');"), "{code}");
        assert!(code.contains("expect(total([1])).toBe(1);"), "{code}");
        assert!(!code.contains("the empty cart"), "{code}");
        assert!(!code.contains("expect(2).toBe(2);"), "{code}");
    }

    /// A doctest lives in comments, and is shown though comments are not.
    #[test]
    fn a_rust_doctest_is_shown_from_its_comments() {
        let consts = (0..12)
            .map(|n| format!("pub const C{n}: u8 = {n};\n"))
            .collect::<String>();
        let text = format!(
            "{consts}/// Adds two numbers.\n///\n/// ```\n/// assert_eq!(shop::add(1, 1), 2);\n/// ```\npub fn add(a: u8, b: u8) -> u8 {{\n    a + b\n}}\n"
        );
        let code = shown_test_code(
            "src/lib.rs",
            &text,
            "src/lib.rs - add (line 15)",
            "rustdoc",
            Language::Rust,
        );
        assert!(
            code.contains("/// assert_eq!(shop::add(1, 1), 2);"),
            "{code}"
        );
        assert!(code.contains("pub fn add(a: u8, b: u8) -> u8 {"), "{code}");
        assert!(!code.contains("Adds two numbers."), "{code}");
    }

    /// A Python test is shown in its own class, not a class nested beside
    /// it; a test on the file's first line is found; and a line above it
    /// that starts with `@` at another depth is no decorator of it.
    #[test]
    fn a_python_test_is_shown_with_its_own_class_and_decorators_alone() {
        let constants = (0..10).map(|i| format!("C{i} = {i}\n")).collect::<String>();
        let nested = format!(
            "{constants}\n\nclass TestCart:\n    def test_total(self):\n        assert cart() == 0\n\n\nclass TestOrder:\n    class TestRefund:\n        def test_total(self):\n            assert refund() == 1\n\n    def test_total(self):\n        assert order() == 2\n"
        );
        let code = shown_test_code(
            "tests/test_a.py",
            &nested,
            "tests/test_a.py::TestOrder::test_total",
            "pytest",
            Language::Python,
        );
        assert!(code.contains("class TestOrder:"), "{code}");
        assert!(code.contains("assert order() == 2"), "{code}");
        for left in ["class TestRefund:", "refund()", "cart()"] {
            assert!(!code.contains(left), "{left}: {code}");
        }
        let first = shown_test_code(
            "tests/test_one.py",
            "def test_first():\n    assert one() == 1\n",
            "tests/test_one.py::test_first",
            "pytest",
            Language::Python,
        );
        assert!(first.contains("assert one() == 1"), "{first}");
        assert!(!first.contains("..."), "{first}");
        let matrix = format!(
            "{constants}\n\nproduct = (a\n    @ b)\ndef test_after():\n    assert product\n"
        );
        let code = shown_test_code(
            "tests/test_m.py",
            &matrix,
            "tests/test_m.py::test_after",
            "pytest",
            Language::Python,
        );
        assert!(code.contains("def test_after():"), "{code}");
        assert!(code.contains("assert product"), "{code}");
        assert!(!code.contains("@ b)"), "{code}");
    }

    /// A JVM test's helpers are the classes it imports, each once: not the
    /// source under assessment, nor a class from `node_modules`.
    #[test]
    fn a_jvm_test_s_helpers_are_its_own_imports_once() {
        let root = project(&[
            (
                "src/test/java/shop/CartTest.java",
                "package shop;\n\nimport shop.Cart;\nimport shop.Fixtures;\nimport shop.Fixtures;\nimport com.facebook.react.Bridge;\nimport static org.junit.Assert.assertEquals;\n\nclass CartTest {\n  @Test\n  void adds() {\n    assertEquals(2, new Cart().total(Fixtures.ONE));\n  }\n}\n",
            ),
            (
                "src/test/java/shop/Fixtures.java",
                "package shop;\n\nclass Fixtures {\n  static final int ONE = 1;\n}\n",
            ),
            (
                "node_modules/react-native/android/com/facebook/react/Bridge.java",
                "package com.facebook.react;\nclass Bridge {}\n",
            ),
        ]);
        let cart =
            "package shop;\n\nclass Cart {\n  int total(int a) {\n    return a + 1;\n  }\n}\n";
        let input = AssessmentInput {
            tests: vec![input_test(
                Some("src/test/java/shop/CartTest.java"),
                "shop.CartTest#adds()",
                "junit-platform",
            )],
            points: vec![point(
                "src/main/java/shop/Cart.java",
                5,
                "return a + 1;",
                &[0],
            )],
            decisions: Vec::new(),
        };
        let sources =
            BTreeMap::from([("src/main/java/shop/Cart.java".to_owned(), cart.to_owned())]);
        let population = population_from(&root, &input, &sources, Language::Jvm).unwrap();
        let helpers = population.helpers(0, 1);
        assert_eq!(
            helpers.keys().collect::<Vec<_>>(),
            ["src/test/java/shop/Fixtures.java"]
        );
        assert!(helpers["src/test/java/shop/Fixtures.java"].contains("static final int ONE = 1;"));
    }

    /// A Ruby test's spec helper, loaded and required, is shown once and
    /// whole when it fits; a support file it requires is not shown again
    /// among the support files its constants name.
    #[test]
    fn a_ruby_test_s_helpers_that_fit_are_shown_once_and_whole() {
        let root = project(&[
            ("lib/m.rb", "def m\n  1\nend\n"),
            ("spec/spec_helper.rb", "require 'rspec'\n"),
            ("spec/support/fake_api.rb", "class FakeApi\nend\n"),
            (
                "spec/support/order_factory.rb",
                "module OrderFactory\nend\n",
            ),
            (
                "spec/a_spec.rb",
                "require 'spec_helper'\nrequire_relative '../lib/m'\nrequire_relative 'support/fake_api'\nRSpec.describe FakeApi do\n  it { expect(OrderFactory.build(m)).to be_ok }\nend\n",
            ),
        ]);
        let input = AssessmentInput {
            tests: vec![input_test(
                Some("spec/a_spec.rb"),
                "spec/a_spec.rb[1:1]",
                "rspec",
            )],
            points: vec![point("lib/m.rb", 2, "1", &[0])],
            decisions: Vec::new(),
        };
        let sources = BTreeMap::from([("lib/m.rb".to_owned(), "def m\n  1\nend\n".to_owned())]);
        let population = population_from(&root, &input, &sources, Language::Ruby).unwrap();
        let helpers = population.helpers(0, 1);
        assert_eq!(
            helpers.keys().collect::<Vec<_>>(),
            [
                "spec/spec_helper.rb",
                "spec/support/fake_api.rb",
                "spec/support/order_factory.rb"
            ]
        );
        assert_eq!(
            helpers["spec/spec_helper.rb"],
            "    1 require 'rspec'\n    2 "
        );
        assert!(!helpers.values().any(|body| body.contains("(cut)")));
    }

    /// When the file has changed since the run, what is left of it near the
    /// statement is shown, its comments aside, and nothing when the
    /// statement now lies past its end.
    #[test]
    fn the_code_run_of_a_file_shorter_than_the_run_shows_what_is_left() {
        let source = (1..=30)
            .map(|n| format!("step{n}();"))
            .collect::<Vec<_>>()
            .join("\n");
        let now = (1..=9)
            .map(|n| format!("step{n}();\n"))
            .chain(std::iter::once("// moved".to_owned()))
            .collect::<String>();
        let shown = |line: usize| {
            let input = AssessmentInput {
                tests: vec![input_test(Some("test/a.test.js"), "a", "vitest")],
                points: vec![point("src/a.js", line, &format!("step{line}();"), &[0])],
                decisions: Vec::new(),
            };
            let sources = BTreeMap::from([("src/a.js".to_owned(), source.clone())]);
            population_from(
                Path::new("/nonexistent"),
                &input,
                &sources,
                Language::JavaScript,
            )
            .unwrap()
            .with_files(BTreeMap::from([("src/a.js".to_owned(), now.clone())]))
            .code_run(0, &[0])
            .remove("src/a.js")
            .unwrap()
        };
        assert_eq!(
            shown(12),
            "        ... 7 lines not shown\n     8 step8();\n     9 step9();"
        );
        assert_eq!(shown(20), "");
    }

    /// Outside JavaScript and Python the other files a test ran are shown
    /// too, largest first, each only if it fits what is left of the budget.
    #[test]
    fn other_files_a_test_ran_are_shown_while_they_fit() {
        let big = format!(
            "package a\n\nfunc g() {{\n{}}}\n",
            (0..60)
                .map(|n| format!("\tvalue{n} := compute({n})\n"))
                .collect::<String>()
        );
        let sources = BTreeMap::from([
            (
                "a.go".to_owned(),
                "package a\n\nfunc f(x int) int {\n\ty := x + 1\n\treturn y\n}\n".to_owned(),
            ),
            ("b.go".to_owned(), big),
            (
                "c.go".to_owned(),
                "package a\n\nfunc h() {\n\tready := compute(0)\n}\n".to_owned(),
            ),
        ]);
        let mut points = vec![point("a.go", 4, "y := x + 1", &[0])];
        points.extend(
            (0..60).map(|n| point("b.go", 4 + n, &format!("value{n} := compute({n})"), &[0])),
        );
        points.push(point("c.go", 4, "ready := compute(0)", &[0]));
        let input = AssessmentInput {
            tests: vec![input_test(Some("a_test.go"), "TestF", "go-test")],
            points,
            decisions: Vec::new(),
        };
        let population = population_from(Path::new("/nonexistent"), &input, &sources, Language::Go)
            .unwrap()
            .with_files(sources.clone());
        let asked = population
            .statements
            .iter()
            .position(|s| s.file == "a.go")
            .unwrap();
        assert_eq!(
            population.ran[0]["b.go"].len(),
            60,
            "b.go ran, and is left out"
        );
        let shown = population.code_run(0, &[asked]);
        assert_eq!(shown.keys().collect::<Vec<_>>(), ["a.go", "c.go"]);
        assert!(
            shown["c.go"].contains("▶    4 \tready := compute(0)"),
            "{shown:?}"
        );
    }

    /// Where no value is written out for a change, each language keeps its
    /// own wording: Go's zero values in general when the function's result
    /// types cannot be read, the JVM's default value whatever a statement
    /// carries, Rust's default (an `Ok` of it for an `Ok`), and Python's
    /// bare `return` and `raise` deleted.
    #[test]
    fn a_change_with_no_value_written_out_keeps_its_language_s_wording() {
        let go = questions_of(
            "a.go",
            "package a\n\nfunc f() {\n\treturn g()\n}\n",
            &[(4, "return g()")],
            Language::Go,
        );
        assert!(
            go[0][0].contains("`return g()` returns the zero value of each result instead"),
            "{go:#?}"
        );

        let population = |file: &str, source: &str, lines: &[(usize, &str)], language| {
            let input = AssessmentInput {
                tests: vec![input_test(Some("t"), "t", "r")],
                points: lines
                    .iter()
                    .map(|(line, text)| point(file, *line, text, &[0]))
                    .collect(),
                decisions: Vec::new(),
            };
            let sources = BTreeMap::from([(file.to_owned(), source.to_owned())]);
            population_from(Path::new("/nonexistent"), &input, &sources, language).unwrap()
        };
        let java = "class A {\n  int g(int x) {\n    int y = x + 1;\n    return y;\n  }\n}\n";
        let mut jvm = population(
            "A.java",
            java,
            &[(3, "int y = x + 1;"), (4, "return y;")],
            Language::Jvm,
        );
        for statement in &mut jvm.statements {
            statement.replacement = Some("0".into());
        }
        assert_eq!(
            jvm.questions(0),
            [
                "A.java:3: `int y = x + 1;`: after it runs, the variable it assigns holds the default value (null, or 0 or false for a primitive) instead."
            ]
        );
        assert_eq!(
            jvm.questions(1),
            [
                "A.java:4: `return y;` returns the default value (null, or 0 or false for a primitive) instead (its expression still runs)."
            ]
        );

        let rust = "fn f(x: u8) -> Result<u8, ()> {\n    return Ok(x);\n}\nfn g(x: u8) -> u8 {\n    return x;\n}\n";
        let mut rust = population(
            "src/a.rs",
            rust,
            &[(2, "return Ok(x);"), (5, "return x;")],
            Language::Rust,
        );
        for statement in &mut rust.statements {
            statement.replacement = None;
        }
        assert_eq!(
            rust.questions(0),
            [
                "src/a.rs:2: `return Ok(x);` returns `Ok(Default::default())` instead (the expression is not evaluated)."
            ]
        );
        assert_eq!(
            rust.questions(1),
            [
                "src/a.rs:5: `return x;` returns `Default::default()` instead (the expression is not evaluated)."
            ]
        );

        let python = questions_of(
            "f.py",
            "def f(v):\n    try:\n        v()\n    except ValueError:\n        raise\n    if v:\n        return\n",
            &[(5, "raise"), (7, "return")],
            Language::Python,
        );
        assert_eq!(
            python,
            [
                [
                    "f.py:5: `raise` is deleted: no error is raised here, and execution carries on with whatever follows it."
                ],
                [
                    "f.py:7: `return` is deleted: the function does not return here, and execution carries on with whatever follows it."
                ],
            ]
        );
    }

    /// Each language's wording for a value made absent and a literal
    /// flipped.
    #[test]
    fn each_change_is_worded_as_its_language_reads_it() {
        assert_eq!(
            questions_of(
                "a.rb",
                "def f(v)\n  x = v.compute\n  x\nend\n",
                &[(2, "x = v.compute")],
                Language::Ruby
            ),
            [
                [
                    "a.rb:2: `x = v.compute`: the value after `x =` becomes nil (it is not evaluated)."
                ]
            ]
        );
        assert_eq!(
            questions_of(
                "f.py",
                "def f(v):\n    y = v()\n    v.n += 1\n    return y\n",
                &[(2, "y = v()"), (3, "v.n += 1")],
                Language::Python
            ),
            [
                ["f.py:2: `y = v()` becomes `y = None` (the value is not evaluated)."],
                ["f.py:3: `v.n += 1` is deleted (it never runs)."],
            ]
        );
        let javascript = questions_of(
            "a.js",
            "export function f(v) {\n  const x = v();\n  if (!v || v(')')) throw new TypeError('no');\n  return true;\n}\n",
            &[
                (2, "const x = v();"),
                (3, "if (!v || v(')')) throw new TypeError('no');"),
                (4, "return true;"),
            ],
            Language::JavaScript,
        );
        assert_eq!(
            javascript[0],
            [
                "a.js:2: `const x = v();` becomes `const x = undefined` (the initializer is not evaluated)."
            ]
        );
        assert_eq!(
            javascript[2],
            ["a.js:4: `return true;` becomes `return false;`."]
        );
    }

    /// The readers' rarer shapes: a helper too long for what is left of the
    /// budget, JVM lines that call rather than declare, a test call with an
    /// underscore in its modifier, and a `}` before an `if` that is no
    /// `else`.
    #[test]
    fn the_readers_take_their_rarer_shapes() {
        let mut rust = vec!["fn big() {"];
        rust.extend(std::iter::repeat_n("    work();", 130));
        rust.extend([
            "}",
            "fn small() {",
            "}",
            "fn t() {",
            "    big(); small();",
            "}",
        ]);
        let t = rust.len() - 3;
        assert_eq!(
            used_definitions(&rust, Language::Rust, t, t + 2),
            [t - 2, t - 1]
        );
        let lines = ["@Test(timeout = 1)", "call(a, () -> {", "});"];
        assert!(definitions(&lines, Language::Jvm).is_empty());
        assert!(is_test_call("test.skip_on_ci('x', () => {"));
        assert_eq!(condition("} if (x) {"), "} if (x) {");
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

#[cfg(test)]
mod helper_shape_tests {
    use super::*;

    /// The line readers the questions are rendered with, each fed the shapes
    /// that take its other ways: block-comment continuations, escaped and
    /// unterminated quotes, braces in fixed text, a division beside a
    /// comment, words that only contain a boolean, Ruby blocks closed on
    /// their own line, and each language's definitions.
    #[test]
    fn the_line_readers_take_each_way() {
        for (line, comment) in [
            ("/* c", true),
            ("*/", true),
            ("*", true),
            ("* doc", true),
            ("*\tdoc", true),
            ("  // x", true),
            ("code", false),
        ] {
            assert_eq!(Language::Rust.comment(line), comment, "{line:?}");
        }
        assert_eq!(
            name_words("parseV2Header getHTTPResponse"),
            ["header", "http", "parse", "response"]
                .map(str::to_owned)
                .into()
        );
        for (text, fragments) in [
            (
                r#"x("a \"quoted\" value here")"#,
                vec![r#"a \"quoted\" value here"#],
            ),
            ("'unterminated text here", vec![]),
            (
                r#""closing } brace text here""#,
                vec!["closing } brace text here"],
            ),
            (r#""${a$b} literal text here""#, vec!["literal text here"]),
            (r#""{x} after brace text""#, vec!["after brace text"]),
        ] {
            assert_eq!(quoted_fragments(text), fragments, "{text}");
        }
        // A `//` in a string is no comment, a lone `/` is division, and a
        // `#` with no space after it is a JavaScript private field.
        for (line, at) in [
            (r#"let s = "a \" // b"; // c"#, 21),
            ("a / b // c", 6),
            ("x # y", 2),
            ("x #y", 4),
        ] {
            assert_eq!(comment_start(line), at, "{line}");
        }
        assert_eq!(flipped_code("is_true(x) || true"), "is_true(x) || false");
        assert_eq!(flipped_code("trueish = false"), "trueish = true");
        assert_eq!(flipped_code("x = True"), "x = False");
        assert_eq!(
            brace_end_within(&["fn a() {", "  let x = 1 / 2; // }", "}", "after"], 0, 10),
            2,
            "a brace in a comment does not close the block"
        );
        let ruby = [
            "it 'x' do x end",
            "describe 'y' do",
            "  it 'z' do",
            "  end",
            "end )",
            "foo {",
            "}",
        ];
        assert_eq!(keyword_end(&ruby, 0), 0);
        assert_eq!(keyword_end(&ruby, 1), 4);
        assert_eq!(keyword_end(&ruby, 5), 6);
        let named = |lines: &[&str], language| {
            definitions(lines, language)
                .into_iter()
                .map(|(name, a, b)| format!("{name} {a}-{b}"))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            named(
                &[
                    "subject{ make }",
                    "subject(:thing) { 1 }",
                    "def self.ready?",
                    "end",
                    "def go!",
                    "end",
                    "let(:item) { 2 }",
                ],
                Language::Ruby
            ),
            [
                "subject 0-0",
                "thing 1-1",
                "ready? 2-3",
                "go! 4-5",
                "item 6-6"
            ]
        );
        assert_eq!(
            named(
                &[
                    "@Test",
                    "public void run(int a) {",
                    "}",
                    "foo(1);",
                    "return x(",
                    "int y = z(",
                    "  fun `helper name`() {}",
                    "val f = { fun inner() }",
                ],
                Language::Jvm
            ),
            ["run 1-2", "helper 6-6", "inner 7-7"]
        );
        assert_eq!(
            named(
                &[
                    "func (s *S) Method() {",
                    "}",
                    "var table = []int{",
                    "}",
                    "type T struct {",
                    "}",
                    "  func inner() {",
                ],
                Language::Go
            ),
            ["Method 0-1", "table 2-3", "T 4-5"]
        );
        // The test's own lines are not its helpers; what a helper names is.
        let rust = [
            "pub(crate) fn helper() {",
            "}",
            "const N: u8 = 1;",
            "#[test]",
            "fn t() {",
            "  helper(); big();",
            "}",
            "fn big() {",
            "  helper();",
            "}",
        ];
        assert_eq!(
            used_definitions(&rust, Language::Rust, 4, 6),
            [0, 1, 7, 8, 9]
        );
        for (text, change, whole) in [
            ("break\n;", Change::Skip, None),
            ("call(\n1)", Change::Skip, Some(2)),
            ("x", Change::Skip, None),
            ("call(\n1)", Change::Invert, None),
        ] {
            let statement = Statement {
                file: "a".into(),
                line: 1,
                text: text.into(),
                change,
                replacement: None,
                tests: vec![],
            };
            assert_eq!(whole_statement(&statement), whole, "{text:?}");
        }
    }
}
