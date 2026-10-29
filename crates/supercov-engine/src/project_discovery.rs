//! Runner/build/project discovery for zero-configuration JavaScript suites.

use crate::fault_fs as fs;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    io,
    path::{Path, PathBuf},
};

use oxc_allocator::Allocator;
use oxc_ast::ast::{
    Argument, ArrayExpressionElement, CallExpression, Expression, ImportDeclarationSpecifier,
    ImportExpression, ImportOrExportKind, Program, Statement,
};
use oxc_ast_visit::{Visit, walk};
use oxc_parser::Parser;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::source_discovery::{
    DiscoveredSourceScope, SourceDiscoveryError, SourceLimitation, SourceScope,
    discover_source_scope,
};

const PLAYWRIGHT_CONFIGS: &[&str] = &[
    "playwright.config.ts",
    "playwright.config.mts",
    "playwright.config.js",
    "playwright.config.mjs",
    "playwright.config.cts",
    "playwright.config.cjs",
];
const VITEST_CONFIGS: &[&str] = &[
    "vitest.config.ts",
    "vitest.config.mts",
    "vitest.config.js",
    "vitest.config.mjs",
    "vitest.config.cts",
    "vitest.config.cjs",
    "vite.config.ts",
    "vite.config.mts",
    "vite.config.js",
    "vite.config.mjs",
];
const JEST_CONFIGS: &[&str] = &[
    "jest.config.ts",
    "jest.config.mts",
    "jest.config.js",
    "jest.config.mjs",
    "jest.config.cts",
    "jest.config.cjs",
];
const TEST_DIRECTORIES: &[&str] = &["test", "tests", "e2e", "spec", "specs"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CoverageProject {
    pub root: PathBuf,
    pub source_roots: Vec<String>,
    pub source_files: Vec<String>,
    pub source_scope: SourceScope,
    pub source_limitations: Vec<SourceLimitation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playwright_config: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vitest_config: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jest_config: Option<PathBuf>,
    pub uses_jest: bool,
    /// The tests bundle sources for a browser through a runner Supercov has
    /// no adapter for -- karma, testem, airtap, zuul, mochify -- so each
    /// script-style file carries the runtime the page otherwise lacks.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub browser_runner: bool,
    pub playwright_module: String,
    pub playwright_test_export: String,
    pub playwright_exports: Vec<String>,
    /// The project's build, when the tests may need its output and the
    /// command does not run it: what a failed run names. Supercov runs the
    /// command it is given and no build of its own.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unrun_build: Vec<String>,
}

#[derive(Debug)]
pub enum ProjectDiscoveryError {
    Source(SourceDiscoveryError),
    Io {
        path: PathBuf,
        source: io::Error,
    },
    NoSourceFiles,
    /// Named roots that left nothing: the roots, quoted as written.
    RootsLeftNothing(String),
}

impl From<SourceDiscoveryError> for ProjectDiscoveryError {
    fn from(value: SourceDiscoveryError) -> Self {
        Self::Source(value)
    }
}

impl std::fmt::Display for ProjectDiscoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Source(error) => write!(formatter, "{error}"),
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::NoSourceFiles => write!(
                formatter,
                "No application source files were discovered. If your sources live somewhere unusual, set SUPERCOV_SOURCE_ROOTS=src,app. Alternatively, Supercov may not support your project layout or test framework yet — if so, please open an issue or PR: https://github.com/supercorp-ai/supercov"
            ),
            Self::RootsLeftNothing(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for ProjectDiscoveryError {}

#[track_caller]
fn package_json(root: &Path) -> Value {
    fs::read(root.join("package.json"))
        .ok()
        .and_then(|contents| serde_json::from_slice(&contents).ok())
        .unwrap_or(Value::Null)
}

fn script<'a>(manifest: &'a Value, name: &str) -> Option<&'a str> {
    manifest.get("scripts")?.get(name)?.as_str()
}

#[track_caller]
fn regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false)
}

fn source_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    let lower = name.to_ascii_lowercase();
    [
        ".js", ".jsx", ".ts", ".tsx", ".cjs", ".cjsx", ".cts", ".ctsx", ".mjs", ".mjsx", ".mts",
        ".mtsx",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

#[track_caller]
fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, ProjectDiscoveryError> {
    let mut entries = fs::read_dir(path)
        .map_err(|source| ProjectDiscoveryError::Io {
            path: path.to_owned(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| ProjectDiscoveryError::Io {
            path: path.to_owned(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

fn parse_program<'a>(
    allocator: &'a Allocator,
    path: &Path,
    source: &'a str,
) -> Option<Program<'a>> {
    let source_type = crate::js_instrumenter::project_source_type(path).ok()?;
    let parsed = Parser::new(allocator, source, source_type).parse();
    parsed.errors.is_empty().then_some(parsed.program)
}

#[derive(Debug, Clone)]
struct TestApiCandidate {
    module: String,
    score: usize,
    test_export: Option<String>,
    exports: Vec<String>,
}

fn imported_test_apis(path: &Path, source: &str) -> Vec<TestApiCandidate> {
    let allocator = Allocator::default();
    let Some(program) = parse_program(&allocator, path, source) else {
        return Vec::new();
    };
    // Aggregate per module across the whole file: a facade's helpers are
    // often imported in their own statement (`import { createTestProduct }
    // from "@acme/fixtures"`), and those names must reach the generated shim
    // even though that statement alone carries no test-API signal.
    let mut by_module = BTreeMap::<String, TestApiCandidate>::new();
    for statement in &program.body {
        let Statement::ImportDeclaration(declaration) = statement else {
            continue;
        };
        if declaration.import_kind == ImportOrExportKind::Type {
            continue;
        }
        let module = declaration.source.value.to_string();
        let candidate = by_module
            .entry(module.clone())
            .or_insert_with(|| TestApiCandidate {
                module,
                score: 0,
                test_export: None,
                exports: Vec::new(),
            });
        for specifier in declaration.specifiers.iter().flatten() {
            let ImportDeclarationSpecifier::ImportSpecifier(specifier) = specifier else {
                continue;
            };
            if specifier.import_kind == ImportOrExportKind::Type {
                continue;
            }
            let imported = specifier.imported.name().to_string();
            let local = specifier.local.name.as_str();
            candidate.exports.push(imported.clone());
            if local == "test" {
                candidate.score += 20;
                candidate.test_export = Some(imported.clone());
            } else if imported.to_ascii_lowercase().ends_with("test") {
                candidate.score += 8;
            }
            if local == "expect" || imported == "expect" {
                candidate.score += 10;
            }
        }
    }
    by_module
        .into_values()
        .filter(|candidate| candidate.score > 0)
        .map(|mut candidate| {
            if candidate.module == "@playwright/test" {
                candidate.score += 100;
            } else if candidate.module.to_ascii_lowercase().contains("playwright") {
                candidate.score += 5;
            }
            candidate
        })
        .collect()
}

fn test_api_candidates(directory: &Path, output: &mut Vec<TestApiCandidate>) {
    let Ok(entries) = read_directory(directory) else {
        return;
    };
    for entry in entries {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name == "node_modules" || name == "results" || name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // A link is not followed, and an entry whose type cannot be read is
        // passed over as one.
        let Some(file_type) = entry.file_type().ok().filter(|kind| !kind.is_symlink()) else {
            continue;
        };
        if file_type.is_dir() {
            test_api_candidates(&path, output);
        } else if file_type.is_file()
            && source_file(&path)
            && let Ok(source) = fs::read_to_string(&path)
        {
            output.extend(imported_test_apis(&path, &source));
        }
    }
}

#[derive(Debug, Clone)]
struct PlaywrightAdapter {
    module: String,
    test_export: String,
    exports: Vec<String>,
}

fn discover_playwright_adapter(root: &Path) -> PlaywrightAdapter {
    let mut candidates = Vec::new();
    for directory in TEST_DIRECTORIES {
        test_api_candidates(&root.join(directory), &mut candidates);
    }
    let mut scores = HashMap::<String, usize>::new();
    for candidate in &candidates {
        *scores.entry(candidate.module.clone()).or_default() += candidate.score;
    }
    let module = scores
        .into_iter()
        .min_by(|(left_module, left_score), (right_module, right_score)| {
            (right_score, left_module).cmp(&(left_score, right_module))
        })
        .map(|(module, _)| module)
        .unwrap_or_else(|| "@playwright/test".into());
    let matching = candidates
        .iter()
        .filter(|candidate| candidate.module == module)
        .collect::<Vec<_>>();
    let test_export = matching
        .iter()
        .filter_map(|candidate| {
            candidate
                .test_export
                .as_ref()
                .map(|export| (candidate.score, export))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, export)| export.clone())
        .unwrap_or_else(|| "test".into());
    let exports = matching
        .iter()
        .flat_map(|candidate| candidate.exports.iter().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    PlaywrightAdapter {
        module,
        test_export,
        exports,
    }
}

fn nested_playwright_configs(root: &Path) -> Vec<PathBuf> {
    fn visit(directory: &Path, depth: usize, found: &mut Vec<PathBuf>) {
        if depth > 4 {
            return;
        }
        let Ok(entries) = read_directory(directory) else {
            return;
        };
        for entry in entries {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with('.') || name == "node_modules" {
                continue;
            }
            let path = entry.path();
            // A link is not followed, and an entry whose type cannot be read is
            // passed over as one.
            let Some(file_type) = entry.file_type().ok().filter(|kind| !kind.is_symlink()) else {
                continue;
            };
            if file_type.is_dir() {
                visit(&path, depth + 1, found);
            } else if file_type.is_file() && playwright_config_name(name) {
                found.push(path);
            }
        }
    }

    let mut found = Vec::new();
    for directory in ["test", "tests", "e2e"] {
        visit(&root.join(directory), 0, &mut found);
    }
    found.sort();
    found
}

fn playwright_config_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    if !lower.starts_with("playwright") || !lower.contains(".config.") {
        return false;
    }
    source_file(Path::new(name))
        && lower[..lower.find(".config.").unwrap_or(0)]
            .chars()
            .all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
            })
}

pub fn expanded_command(root: &Path, command: &[String]) -> String {
    let manifest = package_json(root);
    let executable = command
        .first()
        .and_then(|value| Path::new(value).file_name())
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .trim_end_matches(".cmd")
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    let run_index = command.iter().position(|argument| argument == "run");
    let script_name = run_index
        .and_then(|index| command.get(index + 1))
        .or_else(|| {
            (["npm", "pnpm", "yarn", "bun"].contains(&executable.as_str()) && run_index.is_none())
                .then(|| command.get(1))
                .flatten()
        });
    let joined = command.join(" ");
    if ["npm", "pnpm", "yarn", "bun"].contains(&executable.as_str())
        && !(executable == "bun" && command.get(1).is_some_and(|arg| arg == "test"))
        && let Some(script_name) = script_name
    {
        return format!("{joined} {}", script(&manifest, script_name).unwrap_or(""));
    }
    joined
}

/// The command a package-manager script runs, with the arguments given after
/// the script's name where the package manager puts them, after the script
/// (`npm test -- adds` with `"test": "cargo test"` runs `cargo test adds`).
/// Each argument is quoted, so splitting the result as `sh` does gives it
/// back whole. None when the command runs no script of the package.
pub fn script_command(root: &Path, command: &[String]) -> Option<String> {
    let executable = command
        .first()
        .and_then(|value| Path::new(value).file_name())
        .and_then(|value| value.to_str())?
        .trim_end_matches(".cmd")
        .trim_end_matches(".exe")
        .to_ascii_lowercase();
    if !["npm", "pnpm", "yarn", "bun"].contains(&executable.as_str()) {
        return None;
    }
    let name = match command.iter().position(|argument| argument == "run") {
        Some(run) => run + 1,
        None => 1,
    };
    let manifest = package_json(root);
    let script = script(&manifest, command.get(name)?)?;
    let mut extra = &command[name + 1..];
    // The package manager's own separator is not the script's.
    if extra.first().is_some_and(|argument| argument == "--") {
        extra = &extra[1..];
    }
    let quote = |argument: &String| {
        let plain = !argument.is_empty()
            && argument
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "_./:=,+@%-".contains(c));
        if plain {
            argument.clone()
        } else {
            format!("'{}'", argument.replace('\'', r"'\''"))
        }
    };
    Some(
        std::iter::once(script.to_owned())
            .chain(extra.iter().map(quote))
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn relative_build_output(source: &str) -> bool {
    let mut rest = source;
    let mut relative = false;
    while let Some(stripped) = rest.strip_prefix("../").or_else(|| rest.strip_prefix("./")) {
        relative = true;
        rest = stripped;
    }
    relative
        && matches!(
            rest.split('/').next(),
            Some("dist" | "build" | "out" | "output")
        )
}

fn string_expression<'a>(expression: &'a Expression<'_>) -> Option<&'a str> {
    let Expression::StringLiteral(literal) = expression else {
        return None;
    };
    Some(literal.value.as_str())
}

struct BuildOutputScanner<'a> {
    found: bool,
    build_output_scripts: &'a BTreeSet<String>,
}

fn child_process_launcher(expression: &Expression<'_>) -> bool {
    let name = match expression {
        Expression::Identifier(identifier) => Some(identifier.name.as_str()),
        Expression::StaticMemberExpression(member) => Some(member.property.name.as_str()),
        _ => None,
    };
    name.is_some_and(|name| {
        [
            "spawn",
            "spawnSync",
            "exec",
            "execSync",
            "execFile",
            "execFileSync",
        ]
        .contains(&name)
    })
}

/// The package script a launch runs, whether the call names a program and an
/// argument array (`spawn("npm", ["run", "start"])`) or hands the shell one
/// command string (`execSync("npm run start")`, or `spawn` with `shell`). The
/// string form is how most suites start a server, and it is the form the
/// array-only match missed.
fn launched_package_script<'a>(
    program: &'a str,
    arguments: Option<&'a oxc_ast::ast::ArrayExpression<'a>>,
) -> Option<String> {
    let words: Vec<String> = match arguments {
        Some(arguments) => {
            if !package_manager(program) {
                return None;
            }
            arguments
                .elements
                .iter()
                .map(|element| match element {
                    ArrayExpressionElement::StringLiteral(value) => Some(value.value.to_string()),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()?
        }
        None => {
            let mut tokens = command_tokens(program);
            if tokens.is_empty() || !package_manager(&tokens.remove(0)) {
                return None;
            }
            tokens
        }
    };
    match words.first().map(String::as_str) {
        Some("run") => words.get(1).cloned(),
        Some(_) => words.first().cloned(),
        None => None,
    }
}

fn package_manager(value: &str) -> bool {
    let executable = value
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(value)
        .trim_end_matches(".cmd")
        .trim_end_matches(".exe");
    ["npm", "pnpm", "yarn", "bun"].contains(&executable)
}

fn script_references_build_output(command: &str) -> bool {
    command_tokens(command).iter().any(|token| {
        let token = token.trim_start_matches("../").trim_start_matches("./");
        let mut segments = token.split(['/', '\\']);
        let directory = segments.next();
        // A bare `build` is a subcommand -- `vite build` produces output, it
        // does not consume any -- so only a path reaching INTO the directory
        // means the script runs something already compiled.
        segments.next().is_some() && matches!(directory, Some("dist" | "build" | "out" | "output"))
    })
}

fn build_output_scripts(manifest: &Value) -> BTreeSet<String> {
    manifest
        .get("scripts")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .filter_map(|(name, command)| match command.as_str() {
            Some(command) if script_references_build_output(command) => Some(name.clone()),
            _ => None,
        })
        .collect()
}

impl<'a> Visit<'a> for BuildOutputScanner<'_> {
    fn visit_import_declaration(&mut self, declaration: &oxc_ast::ast::ImportDeclaration<'a>) {
        self.found |= relative_build_output(declaration.source.value.as_str());
        walk::walk_import_declaration(self, declaration);
    }

    fn visit_import_expression(&mut self, expression: &ImportExpression<'a>) {
        if let Some(source) = string_expression(&expression.source) {
            self.found |= relative_build_output(source);
        }
        walk::walk_import_expression(self, expression);
    }

    fn visit_call_expression(&mut self, call: &CallExpression<'a>) {
        if matches!(&call.callee, Expression::Identifier(identifier) if identifier.name == "require")
            && let Some(Argument::StringLiteral(source)) = call.arguments.first()
        {
            self.found |= relative_build_output(source.value.as_str());
        }
        if child_process_launcher(&call.callee)
            && let Some(Argument::StringLiteral(program)) = call.arguments.first()
        {
            let arguments = match call.arguments.get(1) {
                Some(Argument::ArrayExpression(arguments)) => Some(&**arguments),
                _ => None,
            };
            self.found |= launched_package_script(program.value.as_str(), arguments)
                .is_some_and(|script| self.build_output_scripts.contains(&script));
        }
        walk::walk_call_expression(self, call);
    }
}

/// Whether tests consume compiled output directly or launch a package script
/// that does. Either form requires the instrumented build before the runner.
fn tests_require_build_output(root: &Path, manifest: &Value) -> bool {
    fn visit(directory: &Path, build_output_scripts: &BTreeSet<String>) -> bool {
        let Ok(entries) = read_directory(directory) else {
            return false;
        };
        for entry in entries {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name == "node_modules" || name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            // A link is not followed, and an entry whose type cannot be read is
            // passed over as one.
            let Some(file_type) = entry.file_type().ok().filter(|kind| !kind.is_symlink()) else {
                continue;
            };
            if file_type.is_dir() && visit(&path, build_output_scripts) {
                return true;
            }
            if file_type.is_file()
                && source_file(&path)
                && let Ok(source) = fs::read_to_string(&path)
            {
                let allocator = Allocator::default();
                if let Some(program) = parse_program(&allocator, &path, &source) {
                    let mut scanner = BuildOutputScanner {
                        found: false,
                        build_output_scripts,
                    };
                    scanner.visit_program(&program);
                    if scanner.found {
                        return true;
                    }
                }
            }
        }
        false
    }

    let build_output_scripts = build_output_scripts(manifest);
    ["test", "tests", "spec", "specs", "e2e", "__tests__"]
        .iter()
        .any(|directory| visit(&root.join(directory), &build_output_scripts))
}

fn command_tokens(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|character: char| matches!(character, '\'' | '"' | '(' | ')'))
                .trim_end_matches([';', ','])
                .to_ascii_lowercase()
        })
        .collect()
}

/// The configuration file a runner is given in the command: `--config
/// <file>`, `--config=<file>` or `-c <file>` after the runner's name, before
/// the next command of a chain. Case is kept: it is a path.
fn configured_by_command(root: &Path, command: &str, tool: &str) -> Option<PathBuf> {
    let tokens = command
        .split_whitespace()
        .map(|token| token.trim_matches(|character: char| matches!(character, '\'' | '"')))
        .collect::<Vec<_>>();
    let mut in_tool = false;
    for (index, token) in tokens.iter().enumerate() {
        if matches!(*token, "&&" | "||" | ";" | "|") {
            in_tool = false;
            continue;
        }
        let file = token.rsplit(['/', '\\']).next().unwrap_or(token);
        let file = file.trim_end_matches(".cmd").trim_end_matches(".exe");
        if file == tool {
            in_tool = true;
            continue;
        }
        if !in_tool {
            continue;
        }
        let value = match token.strip_prefix("--config=") {
            Some(value) => Some(value),
            None if matches!(*token, "--config" | "-c") => tokens.get(index + 1).copied(),
            None => None,
        };
        if let Some(value) = value
            .map(|value| root.join(value))
            .filter(|path| path.is_file())
        {
            return Some(value);
        }
    }
    None
}

fn has_tool(tokens: &[String], tool: &str) -> bool {
    tokens.iter().any(|token| {
        let file = token.rsplit('/').next().unwrap_or(token);
        file == tool
            || file.strip_suffix(".cmd") == Some(tool)
            || file.strip_suffix(".exe") == Some(tool)
    })
}

/// `tokens` and the tokens of every package script they run, however deep:
/// debug's `npm test` runs `npm run test:browser`, which runs karma.
fn reachable_script_tokens(manifest: &Value, tokens: &[String]) -> Vec<String> {
    let mut all = tokens.to_vec();
    let mut visited = BTreeSet::new();
    let mut index = 0;
    while index < all.len() {
        let runner = all[index]
            .rsplit('/')
            .next()
            .unwrap_or(&all[index])
            .to_owned();
        let named = match runner.as_str() {
            "npm" | "pnpm" | "yarn" | "bun" => {
                let next = all.get(index + 1).map(String::as_str);
                match next {
                    Some("run" | "run-script") => all.get(index + 2).cloned(),
                    Some("test") if runner == "bun" => None,
                    Some("test" | "t") => Some("test".to_owned()),
                    Some(name) if runner != "npm" => Some(name.to_owned()),
                    _ => None,
                }
            }
            _ => None,
        };
        if let Some(name) = named
            && visited.insert(name.clone())
            && let Some(body) = script(manifest, &name)
        {
            all.extend(command_tokens(body));
        }
        index += 1;
    }
    all
}

fn configured_path(
    root: &Path,
    environment: &BTreeMap<String, String>,
    key: &str,
) -> Option<PathBuf> {
    environment.get(key).map(|value| root.join(value))
}

fn first_config(root: &Path, candidates: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .map(|candidate| root.join(candidate))
        .find(|path| regular_file(path))
}

/// The package manager the project's tests were started with, to run its
/// build the same way. A Yarn or pnpm workspace's scripts call the manager
/// that runs them; building through npm changed that for no reason.
fn build_package_manager(command: &[String]) -> String {
    command
        .first()
        .filter(|program| package_manager(program))
        .cloned()
        .unwrap_or_else(|| "npm".into())
}

pub fn discover_coverage_project(
    root: &Path,
    environment: &BTreeMap<String, String>,
    command: &[String],
) -> Result<CoverageProject, ProjectDiscoveryError> {
    let project = discover_coverage_project_of_any_size(root, environment, command)?;
    if project.source_files.is_empty() {
        return Err(no_source_error(root, environment));
    }
    Ok(project)
}

/// Why a project has nothing to measure. Roots that were named and left
/// nothing are the mistake to show, as the other languages show it: "set
/// SUPERCOV_SOURCE_ROOTS" read as if it had not been set.
pub fn no_source_error(
    root: &Path,
    environment: &BTreeMap<String, String>,
) -> ProjectDiscoveryError {
    // Discovery read the same roots first, and refused any it could not.
    match crate::source_discovery::ExplicitSourceRoots::from_environment(root, environment)
        .expect("roots discovery accepted resolve")
    {
        Some(explicit) => ProjectDiscoveryError::RootsLeftNothing(
            explicit.nothing_left("JavaScript or TypeScript"),
        ),
        None => ProjectDiscoveryError::NoSourceFiles,
    }
}

/// `discover_coverage_project` for a suite whose application may hold no
/// JavaScript at all: browser tests of a Python server.
pub fn discover_coverage_project_of_any_size(
    root: &Path,
    environment: &BTreeMap<String, String>,
    command: &[String],
) -> Result<CoverageProject, ProjectDiscoveryError> {
    let manifest = package_json(root);
    let configured_roots = crate::source_discovery::configured_source_roots(environment);
    let DiscoveredSourceScope {
        source_files,
        source_roots,
        scope,
        limitations,
    } = discover_source_scope(root, configured_roots.as_deref())?;
    // The configuration the command names comes before any found by name: a
    // root `playwright.config.ts` beside `--config e2e/playwright.config.ts`
    // made the run load the root one, and run other tests than the command.
    let expanded = expanded_command(root, command);
    let playwright_config = configured_path(root, environment, "SUPERCOV_PLAYWRIGHT_CONFIG")
        .or_else(|| configured_by_command(root, &expanded, "playwright"))
        .or_else(|| first_config(root, PLAYWRIGHT_CONFIGS))
        .or_else(|| nested_playwright_configs(root).into_iter().next());
    let vitest_config = configured_path(root, environment, "SUPERCOV_VITEST_CONFIG")
        .or_else(|| configured_by_command(root, &expanded, "vitest"))
        .or_else(|| first_config(root, VITEST_CONFIGS));
    let jest_config = configured_path(root, environment, "SUPERCOV_JEST_CONFIG")
        .or_else(|| configured_by_command(root, &expanded, "jest"))
        .or_else(|| first_config(root, JEST_CONFIGS));
    let discovered_playwright = discover_playwright_adapter(root);
    let playwright_module = environment
        .get("SUPERCOV_PLAYWRIGHT_MODULE")
        .cloned()
        .unwrap_or_else(|| discovered_playwright.module.clone());
    let playwright_test_export = environment
        .get("SUPERCOV_PLAYWRIGHT_TEST_EXPORT")
        .cloned()
        .unwrap_or_else(|| {
            if playwright_module == discovered_playwright.module {
                discovered_playwright.test_export.clone()
            } else {
                "test".into()
            }
        });
    let expanded_test_command = expanded;
    let tokens = command_tokens(&expanded_test_command);
    let uses_jest =
        jest_config.is_some() || has_tool(&tokens, "jest") || manifest.get("jest").is_some();
    let script_tokens = reachable_script_tokens(&manifest, &tokens);
    let browser_runner = ["playwright", "karma", "testem", "airtap", "zuul", "mochify"]
        .iter()
        .any(|tool| has_tool(&script_tokens, tool));
    let source_transforming_runner = has_tool(&tokens, "jest") || has_tool(&tokens, "vitest");
    let node_test = has_tool(&tokens, "node") && tokens.iter().any(|token| token == "--test");
    let typescript_test = tokens.iter().any(|token| {
        [".ts", ".tsx", ".cts", ".mts"]
            .iter()
            .any(|extension| token.ends_with(extension) || token.contains(&format!("{extension}*")))
    });
    let owns_build = ["vite", "tsc", "webpack", "rollup", "next", "remix"]
        .iter()
        .any(|tool| has_tool(&tokens, tool));
    // Reading the suite is the expensive half of this question, so it stays on
    // the right of `&&`: a command that never executes source directly answers
    // it without parsing a single test file.
    let executes_source_directly = (source_transforming_runner
        || (node_test && typescript_test && !owns_build))
        && !tests_require_build_output(root, &manifest);
    // Supercov runs the command it is given and no build of its own. It used
    // to run the project's `build` script first whenever the tests might
    // need its output: twenty seconds nobody asked for before a suite that
    // builds the application itself, in two of five popular repositories,
    // and a run cancelled when that build failed. The build is only what a
    // failed run names, when the command does not already reach it.
    let build_script = script(&manifest, "build")
        .map(command_tokens)
        .unwrap_or_default();
    let command_builds = !build_script.is_empty()
        && build_script
            .iter()
            .all(|token| script_tokens.contains(token));
    let unrun_build = if !build_script.is_empty() && !command_builds && !executes_source_directly {
        vec![build_package_manager(command), "run".into(), "build".into()]
    } else {
        Vec::new()
    };
    let playwright_exports = if playwright_module == discovered_playwright.module {
        discovered_playwright.exports
    } else {
        vec![playwright_test_export.clone(), "expect".into()]
    };
    Ok(CoverageProject {
        root: root.to_owned(),
        source_roots,
        source_files,
        source_scope: scope,
        source_limitations: limitations,
        playwright_config,
        vitest_config,
        jest_config,
        uses_jest,
        browser_runner,
        playwright_module,
        playwright_test_export,
        playwright_exports,
        unrun_build,
    })
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn fresh_root(label: &str) -> crate::test_directory::TestDirectory {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        crate::test_directory::TestDirectory::adopt(root)
    }

    /// A Playwright configuration is a source file named `playwright` then
    /// plain characters then `.config.`, found under the test directories to
    /// four levels, past hidden directories, `node_modules` and links.
    /// A file named as source the parser has no grammar for parses to
    /// nothing, and adds no test API.
    #[test]
    fn a_source_name_the_parser_does_not_know_parses_to_nothing() {
        assert!(source_file(Path::new("t.mjsx")));
        let allocator = Allocator::default();
        assert!(parse_program(&allocator, Path::new("t.mjsx"), "export const t = 1;\n").is_none());
        assert!(parse_program(&allocator, Path::new("t.mjs"), "export const t = 1;\n").is_some());
        assert!(imported_test_apis(Path::new("t.mjsx"), "export const t = 1;\n").is_empty());
    }

    #[test]
    fn nested_playwright_configurations_are_found_by_name_and_depth() {
        for (name, config) in [
            ("playwright.config.ts", true),
            ("playwright-ct.config.mjs", true),
            ("Playwright.e2e.config.js", true),
            ("playwright.config.json", false),
            ("playwright.ts", false),
            ("jest.config.js", false),
            ("playwright@x.config.ts", false),
        ] {
            assert_eq!(playwright_config_name(name), config, "{name}");
        }
        let root = fresh_root("nested-playwright");
        for file in [
            "tests/a/playwright.config.ts",
            "tests/node_modules/playwright.config.ts",
            "tests/.cache/playwright.config.ts",
            "tests/1/2/3/4/playwright.config.ts",
            "tests/1/2/3/4/5/playwright.config.ts",
            "tests/notes/playwright.config.txt",
            "e2e/playwright.config.js",
            "src/playwright.config.ts",
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "export default {};\n").unwrap();
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("tests/a"), root.join("tests/linked")).unwrap();
        std::fs::create_dir(root.join("tests/empty")).unwrap();
        let found = nested_playwright_configs(&root)
            .into_iter()
            .map(|path| {
                path.strip_prefix(&*root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            [
                "e2e/playwright.config.js",
                "tests/1/2/3/4/playwright.config.ts",
                "tests/a/playwright.config.ts",
            ]
        );
    }

    /// A package manager's command shows the script it runs after it.
    #[test]
    fn a_package_manager_command_is_expanded_with_its_script() {
        let root = fresh_root("expanded-command");
        std::fs::write(
            root.join("package.json"),
            r#"{"scripts": {"test": "vitest run", "e2e": "playwright test"}}"#,
        )
        .unwrap();
        let expanded = |command: &[&str]| {
            expanded_command(
                &root,
                &command
                    .iter()
                    .map(|part| part.to_string())
                    .collect::<Vec<_>>(),
            )
        };
        assert_eq!(expanded(&["npm", "test"]), "npm test vitest run");
        assert_eq!(
            expanded(&["pnpm", "run", "e2e"]),
            "pnpm run e2e playwright test"
        );
        assert_eq!(
            expanded(&["/usr/local/bin/yarn.cmd", "e2e"]),
            "/usr/local/bin/yarn.cmd e2e playwright test"
        );
        assert_eq!(expanded(&["node", "x.js"]), "node x.js");
        assert_eq!(expanded(&["npm"]), "npm");
        // `run` names no script of its own, even with none after it.
        assert_eq!(expanded(&["npm", "run"]), "npm run");
        assert_eq!(expanded(&["npm", "missing"]), "npm missing ");
        // A script started from a command string names the script it runs.
        for (program, script) in [
            ("npm run start", Some("start")),
            ("yarn dev", Some("dev")),
            ("pnpm", None),
            ("", None),
            ("node server.js", None),
        ] {
            assert_eq!(
                launched_package_script(program, None).as_deref(),
                script,
                "{program}"
            );
        }
    }

    /// A script runs with the arguments after its name appended, the package
    /// manager's own `--` left out and each argument kept whole.
    #[test]
    fn a_script_command_carries_the_arguments_after_the_script_name() {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-script-command-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::write(
            root.join("package.json"),
            r#"{ "scripts": { "test": "cargo test", "unit": "cargo test --lib" } }"#,
        )
        .unwrap();
        let words = |line: &str| line.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let command = |line: &str| script_command(&root, &words(line));
        assert_eq!(
            command("npm test -- adds").as_deref(),
            Some("cargo test adds")
        );
        assert_eq!(command("npm run unit").as_deref(), Some("cargo test --lib"));
        assert_eq!(
            command("pnpm run unit adds --release").as_deref(),
            Some("cargo test --lib adds --release")
        );
        let mut quoted = words("npm test --");
        quoted.extend(["two words".into(), "it's".into(), String::new()]);
        assert_eq!(
            script_command(&root, &quoted).as_deref(),
            Some(r"cargo test 'two words' 'it'\''s' ''")
        );
        assert_eq!(command("cargo test"), None);
        assert_eq!(command("npm run missing"), None);
        std::fs::remove_dir_all(&root).unwrap();
    }

    fn project(label: &str, files: &[(&str, &str)]) -> crate::test_directory::TestDirectory {
        // The clock ticks once per microsecond: the counter is what keeps two
        // tests that start together apart.
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-project-{label}-{}-{nonce}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        for (file, contents) in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        root
    }

    fn command(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).into()).collect()
    }

    #[test]
    fn the_configuration_the_command_names_comes_first() {
        // A root configuration beside the one the command names: the run
        // loaded the root one and ran other tests than the command.
        let root = project(
            "named-config",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"test:e2e":"playwright test --config e2e/Playwright.config.ts","test:unit":"vitest run -c config/vitest.unit.ts"}}"#,
                ),
                ("src/main.ts", "export const ready = true"),
                ("playwright.config.ts", "export default {}"),
                ("e2e/Playwright.config.ts", "export default {}"),
                ("vitest.config.ts", "export default {}"),
                ("config/vitest.unit.ts", "export default {}"),
            ],
        );
        let discover = |args: &[&str]| {
            discover_coverage_project(&root, &BTreeMap::new(), &command(args)).unwrap()
        };
        assert_eq!(
            discover(&["npm", "run", "test:e2e"]).playwright_config,
            Some(root.join("e2e/Playwright.config.ts"))
        );
        assert_eq!(
            discover(&[
                "npx",
                "playwright",
                "test",
                "--config=e2e/Playwright.config.ts"
            ])
            .playwright_config,
            Some(root.join("e2e/Playwright.config.ts"))
        );
        assert_eq!(
            discover(&["npm", "run", "test:unit"]).vitest_config,
            Some(root.join("config/vitest.unit.ts"))
        );
        // Named for another tool, or not at all: found by name as before.
        assert_eq!(
            discover(&["npx", "playwright", "test"]).playwright_config,
            Some(root.join("playwright.config.ts"))
        );
        assert_eq!(
            discover(&["npx", "vitest", "run", "--config", "missing.ts"]).vitest_config,
            Some(root.join("vitest.config.ts"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_conventional_vite_playwright_and_vitest_configuration() {
        let root = project(
            "vite",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"vite build"},"devDependencies":{"vite":"1"}}"#,
                ),
                ("src/main.ts", "export const ready = true"),
                ("playwright.config.ts", "export default {}"),
                ("vitest.config.ts", "export default {}"),
                (
                    "tests/example.spec.ts",
                    "import { test } from '@playwright/test'",
                ),
            ],
        );
        let discovered = discover_coverage_project(&root, &BTreeMap::new(), &[]).unwrap();
        assert_eq!(discovered.source_roots, ["src"]);
        assert_eq!(
            discovered.playwright_config,
            Some(root.join("playwright.config.ts"))
        );
        assert_eq!(
            discovered.vitest_config,
            Some(root.join("vitest.config.ts"))
        );
        assert_eq!(discovered.playwright_module, "@playwright/test");
        assert_eq!(discovered.playwright_test_export, "test");
        assert_eq!(discovered.playwright_exports, ["test"]);
        // The command runs as given: the project\'s build is only what a
        // failed run names.
        assert_eq!(discovered.unrun_build, command(&["npm", "run", "build"]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_browser_runner_the_tests_reach_through_nested_scripts_is_found() {
        // debug's `npm test` runs `npm run test:browser`, which runs karma.
        let root = project(
            "karma",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"test":"npm run test:node && npm run test:browser","test:node":"mocha test.js","test:browser":"karma start --single-run"}}"#,
                ),
                ("src/index.js", "module.exports = 1"),
            ],
        );
        let command = ["npm".to_owned(), "test".to_owned()];
        let discovered = discover_coverage_project(&root, &BTreeMap::new(), &command).unwrap();
        assert!(discovered.browser_runner);
        let node_only = ["npm".to_owned(), "run".to_owned(), "test:node".to_owned()];
        let discovered = discover_coverage_project(&root, &BTreeMap::new(), &node_only).unwrap();
        assert!(!discovered.browser_runner);
    }

    #[test]
    fn browser_config_and_commands_select_the_runtime_for_the_suite() {
        let root = project(
            "browser-and-unit",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"test":"npm run unit && npm run browser","unit":"bun test","browser":"playwright test"}}"#,
                ),
                ("src/main.ts", "export const ready = true"),
                ("playwright.config.ts", "export default {}"),
            ],
        );
        let discover = |args: &[&str]| {
            discover_coverage_project(&root, &BTreeMap::new(), &command(args)).unwrap()
        };
        assert!(discover(&["npm", "test"]).browser_runner);
        assert!(!discover(&["bun", "test"]).browser_runner);
        assert!(!discover(&["node", "--test"]).browser_runner);
        // Config discovery retains browser initialization even for an opaque
        // wrapper, without pretending to know which runner it invokes.
        let opaque = discover(&["./run-tests"]);
        assert!(opaque.playwright_config.is_some());
        assert!(!opaque.browser_runner);
        // A package's "test" script can name an unrelated browser suite
        // even when no browser config exists. `bun test` runs Bun itself;
        // `bun run test` and `npm test` run that script.
        fs::remove_file(root.join("playwright.config.ts")).unwrap();
        assert_eq!(
            expanded_command(&root, &command(&["bun", "test"])),
            "bun test"
        );
        assert!(!discover(&["bun", "test"]).browser_runner);
        assert!(discover(&["bun", "run", "test"]).browser_runner);
        assert!(discover(&["npm", "test"]).browser_runner);
    }

    #[test]
    fn skips_unrelated_builds_for_source_transforming_and_node_test_suites() {
        for (label, test_script, test_file) in [
            ("jest", "jest", "require('../src/index.ts')"),
            ("vitest", "vitest run", "import '../src/index.ts'"),
            (
                "node",
                "node --test tests/*.test.ts",
                "import '../src/index.ts'",
            ),
        ] {
            let root = project(
                label,
                &[
                    (
                        "package.json",
                        &format!(
                            r#"{{"scripts":{{"build":"node build","test":"{test_script}"}}}}"#
                        ),
                    ),
                    ("src/index.ts", "export const ready = true"),
                    ("tests/index.test.ts", test_file),
                ],
            );
            let discovered =
                discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"]))
                    .unwrap();
            assert!(discovered.unrun_build.is_empty(), "{label}");
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn names_the_build_with_the_package_manager_the_tests_were_started_with() {
        let root = project(
            "yarn-build",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"tsc","test":"jest --runInBand"}}"#,
                ),
                ("src/index.ts", "export const ready = true"),
                ("test/index.test.js", "require('../dist/index.js')"),
            ],
        );
        let discovered =
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["yarn", "test"]))
                .unwrap();
        assert_eq!(discovered.unrun_build, command(&["yarn", "run", "build"]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_the_build_when_tests_import_compiled_output() {
        let root = project(
            "compiled",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"tsc","test":"jest --runInBand"}}"#,
                ),
                ("src/index.ts", "export const ready = true"),
                ("test/index.test.js", "require('../dist/index.js')"),
            ],
        );
        let discovered =
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"])).unwrap();
        assert_eq!(discovered.unrun_build, command(&["npm", "run", "build"]));
        assert!(discovered.uses_jest);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_the_build_when_source_direct_tests_spawn_a_compiled_package_script() {
        let root = project(
            "spawned-compiled-script",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"tsc","start":"node dist/index.js","test":"node --test tests/*.test.ts"}}"#,
                ),
                ("src/index.ts", "export const ready = true"),
                (
                    "tests/index.test.ts",
                    "import { spawn } from 'node:child_process';\nspawn('npm', ['run', 'start']);\n",
                ),
            ],
        );
        let discovered =
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"])).unwrap();
        assert_eq!(discovered.unrun_build, command(&["npm", "run", "build"]));
        fs::remove_dir_all(root).unwrap();
    }

    /// A script named by a variable is no script the discovery can name, so
    /// no build is named for it.
    #[test]
    fn a_package_script_named_by_a_variable_names_no_build() {
        let root = project(
            "spawned-variable-script",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"tsc","start":"node dist/index.js","test":"node --test tests/*.test.ts"}}"#,
                ),
                ("src/index.ts", "export const ready = true"),
                (
                    "tests/index.test.ts",
                    "import { spawn } from 'node:child_process';\nconst script = 'start';\nspawn('npm', ['run', script]);\n",
                ),
            ],
        );
        let discovered =
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"])).unwrap();
        assert!(
            discovered.unrun_build.is_empty(),
            "{:?}",
            discovered.unrun_build
        );
        // A directory the walk cannot list, or an entry of it, fails it.
        for fault in ["fs.read_dir", "fs.read_dir entry"] {
            assert!(
                crate::fault::during(fault, || {
                    discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"]))
                })
                .is_err(),
                "{fault}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn names_the_build_when_tests_exec_a_compiled_package_script_as_one_string() {
        // `execSync("npm run start")` hands the shell a single string. It is
        // how most suites start the server they test against, and the
        // array-only match let it through to the same hang as an unbuilt
        // gateway.
        let root = project(
            "exec-compiled-script",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"tsc","start":"node dist/index.js","test":"node --test tests/*.test.ts"}}"#,
                ),
                ("src/index.ts", "export const ready = true"),
                (
                    "tests/index.test.ts",
                    "import { execSync } from 'node:child_process';\nexecSync('npm run start');\n",
                ),
            ],
        );
        let discovered =
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"])).unwrap();
        assert_eq!(discovered.unrun_build, command(&["npm", "run", "build"]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keeps_source_direct_when_a_spawned_script_only_runs_a_build_subcommand() {
        let root = project(
            "spawned-build-subcommand",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"tsc","dev":"vite build","test":"node --test tests/*.test.ts"}}"#,
                ),
                ("src/index.ts", "export const ready = true"),
                (
                    "tests/index.test.ts",
                    "import { spawn } from 'node:child_process';\nspawn('npm', ['run', 'dev']);\n",
                ),
            ],
        );
        let discovered =
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"])).unwrap();
        assert!(discovered.unrun_build.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_a_project_owned_playwright_fixture_via_the_ast() {
        let root = project(
            "fixture",
            &[
                ("package.json", r#"{"scripts":{"build":"vite build"}}"#),
                ("app/root.tsx", "export default null"),
                (
                    "tests/nested/playwright.browser.config.ts",
                    "export default {}",
                ),
                (
                    "tests/example.spec.ts",
                    "import { type Ignored, browserTest as test, expect, fixtureValue } from '@acme/browser-fixtures'\n\
                     import { createFixtureProduct } from '@acme/browser-fixtures'",
                ),
            ],
        );
        let discovered = discover_coverage_project(&root, &BTreeMap::new(), &[]).unwrap();
        assert_eq!(
            discovered.playwright_config,
            Some(root.join("tests/nested/playwright.browser.config.ts"))
        );
        assert_eq!(discovered.playwright_module, "@acme/browser-fixtures");
        assert_eq!(discovered.playwright_test_export, "browserTest");
        // The helper imported in its own statement must reach the shim: a
        // facade's non-test exports vanish otherwise, and every spec importing
        // one fails to link.
        assert_eq!(
            discovered.playwright_exports,
            [
                "browserTest",
                "createFixtureProduct",
                "expect",
                "fixtureValue"
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// An import scores for a `test` export by name, for `expect` by either
    /// name, and for a module named like Playwright; the search skips what
    /// is not the project's own source.
    #[test]
    fn test_apis_are_scored_by_what_they_import_and_found_in_project_source() {
        let candidates = imported_test_apis(
            Path::new("a.spec.ts"),
            "import { e2eTest, expect as check } from 'my-playwright-kit'\nimport { helper } from './helper'\n",
        );
        assert_eq!(candidates.len(), 1, "{candidates:?}");
        assert_eq!(candidates[0].module, "my-playwright-kit");
        assert_eq!(candidates[0].score, 8 + 10 + 5);
        assert_eq!(candidates[0].test_export, None);
        assert!(imported_test_apis(Path::new("empty.spec.ts"), "").is_empty());
        let root = project(
            "apis",
            &[
                ("e2e/a.spec.ts", "import { test } from '@acme/kit'"),
                (
                    "node_modules/x/a.ts",
                    "import { test } from '@skip/modules'",
                ),
                ("results/a.ts", "import { test } from '@skip/results'"),
                (".cache/a.ts", "import { test } from '@skip/hidden'"),
                ("e2e/notes.txt", "import { test } from '@skip/text'"),
            ],
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("results"), root.join("linked")).unwrap();
        fs::create_dir(root.join("e2e/empty")).unwrap();
        let mut found = Vec::new();
        test_api_candidates(&root, &mut found);
        assert_eq!(
            found.iter().map(|c| c.module.as_str()).collect::<Vec<_>>(),
            ["@acme/kit"]
        );
        test_api_candidates(&root.join("absent"), &mut found);
        assert_eq!(found.len(), 1);
    }

    /// The suite is read past everything that is not a test of its own --
    /// hidden folders, installed packages, links, files that are not text or
    /// do not parse, and loads and launches by a name only known at run time
    /// -- and a Jest started as `jest.cmd` still reads source directly.
    #[test]
    fn a_suite_with_nothing_compiled_runs_source_directly_whatever_else_it_holds() {
        let root = project(
            "source-direct-oddities",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"tsc","test":"node_modules/.bin/jest.cmd"}}"#,
                ),
                ("src/index.ts", "export const ready = true"),
                ("test/.cache/old.test.js", "require('../dist/index.js')"),
                (
                    "test/node_modules/dep/index.js",
                    "require('../dist/index.js')",
                ),
                (
                    "test/dynamic.test.js",
                    "const name = './x';\nrequire(name);\nimport(name);\nimport('./fixtures/y.js');\n",
                ),
                (
                    "test/launch.test.js",
                    "import { spawn } from 'node:child_process';\nconst program = 'npm';\nspawn(program, ['run', 'start']);\nspawn('node', ['dist/index.js']);\n",
                ),
                ("test/broken.test.js", "const = ;\n"),
            ],
        );
        fs::write(root.join("test/latin1.test.js"), b"// caf\xe9\n").unwrap();
        fs::create_dir(root.join("test/empty")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                root.join("test/dynamic.test.js"),
                root.join("test/linked.test.js"),
            )
            .unwrap();
            let fifo = root.join("test/pipe.test.js");
            assert!(
                std::process::Command::new("mkfifo")
                    .arg(&fifo)
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let discovered =
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"])).unwrap();
        assert!(discovered.uses_jest);
        assert!(
            discovered.unrun_build.is_empty(),
            "{:?}",
            discovered.unrun_build
        );
    }

    /// Compiled output a test in a nested folder loads is found there, and a
    /// Windows `.exe` Jest is Jest.
    #[test]
    fn compiled_output_a_nested_test_loads_names_the_build() {
        let root = project(
            "nested-compiled",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"build":"tsc","test":"jest.exe"}}"#,
                ),
                ("src/index.ts", "export const ready = true"),
                (
                    "test/unit/deep/a.test.js",
                    "require('../../../dist/index.js')",
                ),
            ],
        );
        let discovered =
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"])).unwrap();
        assert!(discovered.uses_jest);
        assert_eq!(discovered.unrun_build, command(&["npm", "run", "build"]));
    }

    /// node:test over TypeScript runs source directly unless the command
    /// builds it first; a glob like `*.ts*` names TypeScript too.
    #[test]
    fn node_test_over_typescript_reads_source_unless_the_command_builds() {
        let discovered = |test: &str| {
            let root = project(
                "node-typescript",
                &[
                    (
                        "package.json",
                        &format!(r#"{{"scripts":{{"build":"tsc","test":"{test}"}}}}"#),
                    ),
                    ("src/index.ts", "export const ready = true"),
                ],
            );
            discover_coverage_project(&root, &BTreeMap::new(), &command(&["npm", "test"]))
                .unwrap()
                .unrun_build
        };
        assert!(discovered("node --test 'tests/**/*.ts*'").is_empty());
        // The command runs tsc itself: it owns its build, and none is named.
        assert!(discovered("tsc && node --test tests/a.test.ts").is_empty());
    }

    #[test]
    fn a_run_with_no_script_name_and_an_overridden_playwright_module() {
        let root = project(
            "no-script",
            &[
                ("package.json", r#"{"scripts":{}}"#),
                ("src/a.js", "export const a = 1;\n"),
            ],
        );
        // `npm run` alone names no script.
        assert_eq!(script_command(&root, &command(&["npm", "run"])), None);
        // Nor does a program with no name, or no program.
        assert_eq!(script_command(&root, &command(&[".."])), None);
        assert_eq!(script_command(&root, &[]), None);
        let environment = BTreeMap::from([(
            "SUPERCOV_PLAYWRIGHT_MODULE".to_owned(),
            "./fixtures/playwright.js".to_owned(),
        )]);
        let discovered = discover_coverage_project(
            &root,
            &environment,
            &command(&["npx", "playwright", "test"]),
        )
        .unwrap();
        assert_eq!(discovered.playwright_module, "./fixtures/playwright.js");
        // A module of the user's own exports Playwright's `test` by that name.
        assert_eq!(discovered.playwright_test_export, "test");
    }

    #[test]
    fn environment_overrides_are_authoritative() {
        let root = project(
            "override",
            &[
                ("package.json", r#"{"scripts":{"build":"vite build"}}"#),
                ("custom/main.ts", "main"),
                ("configs/browser.ts", "config"),
                (
                    "tests/example.spec.ts",
                    "import { test } from '@playwright/test'",
                ),
            ],
        );
        let environment = BTreeMap::from([
            ("SUPERCOV_SOURCE_ROOTS".into(), "custom".into()),
            (
                "SUPERCOV_PLAYWRIGHT_CONFIG".into(),
                "configs/browser.ts".into(),
            ),
            ("SUPERCOV_PLAYWRIGHT_MODULE".into(), "@custom/test".into()),
            ("SUPERCOV_PLAYWRIGHT_TEST_EXPORT".into(), "scenario".into()),
        ]);
        let discovered = discover_coverage_project(&root, &environment, &[]).unwrap();
        assert_eq!(discovered.source_roots, ["custom"]);
        assert_eq!(
            discovered.playwright_config,
            Some(root.join("configs/browser.ts"))
        );
        assert_eq!(discovered.playwright_module, "@custom/test");
        assert_eq!(discovered.playwright_test_export, "scenario");
        assert_eq!(discovered.playwright_exports, ["scenario", "expect"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn each_project_discovery_error_says_what_failed() {
        use super::ProjectDiscoveryError as E;
        let messages = [
            E::from(crate::source_discovery::SourceDiscoveryError::InvalidRoot(
                std::path::PathBuf::from("src"),
            )),
            E::Io {
                path: std::path::PathBuf::from("a"),
                source: std::io::Error::other("disk full"),
            },
            E::NoSourceFiles,
            E::RootsLeftNothing("nothing left".into()),
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                "source root is not a regular file or directory: src",
                "a: disk full",
                "No application source files were discovered. If your sources live somewhere unusual, set SUPERCOV_SOURCE_ROOTS=src,app. Alternatively, Supercov may not support your project layout or test framework yet — if so, please open an issue or PR: https://github.com/supercorp-ai/supercov",
                "nothing left",
            ]
        );
    }
}
