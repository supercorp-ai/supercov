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

#[derive(Debug, Clone)]
pub struct Statement {
    pub file: String,
    pub line: usize,
    pub text: String,
    pub change: Change,
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
    pub statements: Vec<Statement>,
    pub tests: Vec<TestRef>,
    /// Per test: the statement lines it ran, by file.
    ran: Vec<BTreeMap<String, BTreeSet<usize>>>,
    /// Files holding assessed statements; a test's own helpers are the files
    /// it imports that are not among these.
    source_files: BTreeSet<String>,
    aliases: Vec<(String, String)>,
    /// Files read once: every request and every cache key reads the same text.
    files: std::sync::Mutex<BTreeMap<String, Option<std::sync::Arc<str>>>>,
}

/// The executed statements of a JavaScript or TypeScript run and the tests that
/// ran them. `sources` are the run's source files, verified against the run.
pub fn population(
    root: &Path,
    report: &CoverageReport,
    sources: &BTreeMap<String, String>,
) -> Result<Population, String> {
    let view = &report.filters.passed;
    let mut tests = Vec::new();
    let mut index = BTreeMap::new();
    for test in &view.tests {
        let Some(file) = test.file.as_ref().filter(|f| !f.is_empty()) else {
            continue;
        };
        if test.role != "test" {
            continue;
        }
        index.insert(test.id.as_str(), tests.len());
        tests.push(TestRef {
            id: test.id.clone(),
            file: file.clone(),
            name: test.name.clone(),
            runner: test.provenance.runner.clone(),
        });
    }
    let mut changes = BTreeMap::<String, BTreeMap<usize, Vec<(String, Option<Change>)>>>::new();
    let mut statements = Vec::new();
    let mut seen = BTreeSet::new();
    for point in &view.points {
        if !point.measured || point.meta.kind != PointKind::Statement {
            continue;
        }
        let key = (point.meta.file.clone(), point.meta.line);
        if !seen.insert(key) {
            continue; // the first statement on a line, as the audit reads it
        }
        let ids = point
            .tests
            .iter()
            .filter_map(|id| index.get(id.as_str()).copied())
            .collect::<Vec<_>>();
        if ids.is_empty() {
            continue;
        }
        let Some(source) = sources.get(&point.meta.file) else {
            continue;
        };
        let lines = changes
            .entry(point.meta.file.clone())
            .or_insert_with(|| statement_starts(&point.meta.file, source));
        let Some(change) = change_at(lines, point.meta.line, &point.meta.source) else {
            continue;
        };
        statements.push(Statement {
            file: point.meta.file.clone(),
            line: point.meta.line,
            text: point.meta.source.clone(),
            change,
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
        statements,
        tests,
        ran,
        source_files,
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
    match found {
        Some((_, change)) => *change,
        None => Some(Change::ExpressionUndefined),
    }
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

fn is_comment(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || t.starts_with("/*")
        || t.starts_with("*/")
        || t == "*"
        || t.starts_with("* ")
        || t.starts_with("*\t")
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

fn describe(name: &str) -> String {
    let parts = name.split(" > ").collect::<Vec<_>>();
    parts[..parts.len().saturating_sub(1)].join(" > ")
}

impl Population {
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
            let g = (self.tests[t].file.clone(), describe(&self.tests[t].name));
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
            .filter(|&i| !is_comment(lines[i]))
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
            if !is_comment(l) {
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

    /// Test-side files the test imports (helpers, fixtures), cut to a budget
    /// that grows with the number of questions.
    pub fn helpers(&self, test: usize, questions: usize) -> BTreeMap<String, String> {
        let cap = HELPER_CHARS.min(1500 + 400 * questions);
        let t = &self.tests[test];
        let text = self.read(&t.file).unwrap_or_default();
        let mut out = BTreeMap::new();
        let mut size = 0;
        for spec in import_specifiers(&text) {
            let Some(file) = self.resolve(&t.file, &spec) else {
                continue;
            };
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
                .filter(|&i| !is_comment(text[i - 1]))
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
                if i == 0 || i > text.len() || is_comment(text[i - 1]) {
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
        let changes = match s.change {
            Change::ReturnUndefined => vec![format!(
                "`{line}` becomes `return undefined;` (the expression is not evaluated)"
            )],
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
                let c = condition(line);
                vec![
                    format!(
                        "`{line}`: whenever the condition `{c}` is true it is treated as false (the branch it would enter is skipped)"
                    ),
                    format!(
                        "`{line}`: whenever the condition `{c}` is false it is treated as true (the branch is entered)"
                    ),
                ]
            }
        };
        changes
            .into_iter()
            .map(|c| format!("{}:{}: {c}.", s.file, s.line))
            .collect()
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
        assert_eq!(change_at(&starts, 1, "import x from 'y'"), None);
        assert_eq!(
            change_at(&starts, 2, "const a = 1"),
            Some(Change::ValueUndefined)
        );
        assert_eq!(change_at(&starts, 3, "let b: number"), None);
        assert_eq!(
            change_at(&starts, 5, "if (a) return 2"),
            Some(Change::Invert)
        );
        assert_eq!(change_at(&starts, 6, "return;"), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 8, "a && b"),
            Some(Change::ExpressionUndefined)
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
