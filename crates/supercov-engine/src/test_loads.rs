//! Which first-party files a JavaScript or TypeScript test file loads, read
//! from its imports, and which test runner a configuration file belongs to.
//!
//! A test's record says which source it ran, never which helpers it loaded:
//! helpers sit beside the tests, outside what is measured. Without this a
//! helper of the end-to-end suite changing made every unit run out of date,
//! and a helper a test did call changing left `tests affected` saying no test
//! was affected.
//!
//! What cannot be read is said: an import whose path is computed, or one that
//! resolves to nothing Supercov can name, leaves the answer open, and an open
//! answer is "it may load anything".
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Component, Path, PathBuf},
};

use serde_json::Value;

/// The files a set of entry files loads, themselves included, or why that
/// cannot be known.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Loads {
    pub files: BTreeSet<String>,
    /// The first import that could not be followed, where one could not.
    pub open: Option<String>,
}

#[derive(Debug, Clone)]
enum Specifier {
    Literal(String),
    /// `import(name)`, `require(path.join(...))`: anything at all.
    Computed,
}

const EXTENSIONS: [&str; 9] = [
    ".ts", ".tsx", ".mts", ".cts", ".js", ".jsx", ".mjs", ".cjs", ".json",
];

/// A file Supercov can read imports from.
pub fn reads_imports(file: &str) -> bool {
    EXTENSIONS[..8]
        .iter()
        .any(|extension| file.ends_with(extension))
}

/// Imports of the files of one checkout, read once each.
pub struct Imports {
    root: PathBuf,
    packages: BTreeSet<String>,
    /// tsconfig `paths`: a pattern with at most one `*`, and where it maps.
    aliases: Vec<(String, Vec<PathBuf>)>,
    /// package.json `imports`, the `#name` specifiers.
    subpaths: BTreeMap<String, Vec<String>>,
    parsed: RefCell<BTreeMap<String, Option<Vec<Specifier>>>>,
}

impl Imports {
    pub fn new(root: &Path) -> Self {
        let manifest = read_json(&root.join("package.json"));
        let mut packages = BTreeSet::new();
        let mut subpaths = BTreeMap::new();
        if let Some(manifest) = &manifest {
            for field in [
                "dependencies",
                "devDependencies",
                "peerDependencies",
                "optionalDependencies",
            ] {
                if let Some(names) = manifest.get(field).and_then(Value::as_object) {
                    packages.extend(names.keys().cloned());
                }
            }
            if let Some(name) = manifest.get("name").and_then(Value::as_str) {
                packages.insert(name.to_owned());
            }
            if let Some(imports) = manifest.get("imports").and_then(Value::as_object) {
                for (name, target) in imports {
                    let mut targets = Vec::new();
                    collect_strings(target, &mut targets);
                    subpaths.insert(name.clone(), targets);
                }
            }
        }
        Self {
            root: root.to_owned(),
            packages,
            aliases: aliases(root),
            subpaths,
            parsed: RefCell::new(BTreeMap::new()),
        }
    }

    /// Everything `entries` load, followed through first-party files.
    pub fn loads<'e>(&self, entries: impl IntoIterator<Item = &'e str>) -> Loads {
        let mut loads = Loads::default();
        let mut queue = entries
            .into_iter()
            .map(str::to_owned)
            .collect::<VecDeque<_>>();
        while let Some(file) = queue.pop_front() {
            if !loads.files.insert(file.clone()) || !reads_imports(&file) {
                continue;
            }
            let Some(specifiers) = self.specifiers(&file) else {
                loads
                    .open
                    .get_or_insert_with(|| format!("{file} could not be read"));
                continue;
            };
            for specifier in specifiers {
                match specifier {
                    Specifier::Computed => {
                        loads
                            .open
                            .get_or_insert_with(|| format!("{file} imports a computed path"));
                    }
                    Specifier::Literal(specifier) => match self.resolve(&file, &specifier) {
                        Resolved::File(next) => queue.push_back(next),
                        Resolved::Outside => {}
                        Resolved::Unknown => {
                            loads.open.get_or_insert_with(|| {
                                format!("{file} imports '{specifier}', which Supercov cannot find")
                            });
                        }
                    },
                }
            }
        }
        loads
    }

    /// Files a runner's configuration names by path, such as a setup file or
    /// a global setup, where one exists: they load before every test. A
    /// pattern names test files, which each run's record already holds.
    pub fn named_by(&self, configuration: &str) -> Vec<String> {
        let Ok(source) = fs::read_to_string(self.root.join(configuration)) else {
            return Vec::new();
        };
        let directory = Path::new(configuration).parent().unwrap_or(Path::new(""));
        let mut named = Vec::new();
        for text in strings(configuration, &source) {
            if !(text.starts_with('.') || text.contains('/')) || text.contains(['*', '{', '?']) {
                continue;
            }
            for candidate in [directory.join(&text), PathBuf::from(&text)] {
                if let Resolved::File(file) = self.file(&candidate)
                    && !named.contains(&file)
                {
                    named.push(file);
                    break;
                }
            }
        }
        named
    }

    fn specifiers(&self, file: &str) -> Option<Vec<Specifier>> {
        if let Some(known) = self.parsed.borrow().get(file) {
            return known.clone();
        }
        let parsed = fs::read_to_string(self.root.join(file))
            .ok()
            .and_then(|source| parse(file, &source));
        self.parsed
            .borrow_mut()
            .insert(file.to_owned(), parsed.clone());
        parsed
    }

    fn resolve(&self, from: &str, specifier: &str) -> Resolved {
        let specifier = specifier.split(['?', '#']).next().unwrap_or(specifier);
        if specifier.starts_with("./") || specifier.starts_with("../") || specifier == "." {
            let base = Path::new(from).parent().unwrap_or(Path::new(""));
            return self.file(&base.join(specifier));
        }
        if specifier.starts_with('/') {
            return Resolved::Unknown;
        }
        if let Some(name) = specifier.strip_prefix("node:") {
            let _ = name;
            return Resolved::Outside;
        }
        if specifier.starts_with('#') {
            return self.subpath(specifier);
        }
        for (pattern, targets) in &self.aliases {
            let rest = match pattern.split_once('*') {
                Some((prefix, suffix)) => specifier
                    .strip_prefix(prefix)
                    .and_then(|rest| rest.strip_suffix(suffix)),
                None => (specifier == pattern).then_some(""),
            };
            let Some(rest) = rest else { continue };
            for target in targets {
                let target = target.to_string_lossy().replacen('*', rest, 1);
                if let Resolved::File(file) = self.file(Path::new(&target)) {
                    return Resolved::File(file);
                }
            }
        }
        let package = package_name(specifier);
        // Installed where Node looks: beside the project or above it.
        if BUILTINS.contains(&package)
            || self.packages.contains(package)
            || self
                .root
                .ancestors()
                .any(|directory| directory.join("node_modules").join(package).exists())
        {
            return Resolved::Outside;
        }
        Resolved::Unknown
    }

    fn subpath(&self, specifier: &str) -> Resolved {
        let targets = self.subpaths.get(specifier).cloned().or_else(|| {
            self.subpaths.iter().find_map(|(pattern, targets)| {
                let (prefix, suffix) = pattern.split_once('*')?;
                let rest = specifier.strip_prefix(prefix)?.strip_suffix(suffix)?;
                Some(
                    targets
                        .iter()
                        .map(|target| target.replacen('*', rest, 1))
                        .collect(),
                )
            })
        });
        let Some(targets) = targets else {
            return Resolved::Unknown;
        };
        for target in targets {
            if !target.starts_with("./") {
                return Resolved::Outside;
            }
            if let Resolved::File(file) = self.file(Path::new(&target)) {
                return Resolved::File(file);
            }
        }
        Resolved::Unknown
    }

    /// A first-party file at `path` (relative to the root), the way Node,
    /// TypeScript and bundlers find it: as named, with an extension, as a
    /// directory's index, and a `.js` that names its TypeScript source.
    fn file(&self, path: &Path) -> Resolved {
        let Some(path) = normal(path) else {
            return Resolved::Unknown;
        };
        if path.split('/').any(|part| part == "node_modules") {
            return Resolved::Outside;
        }
        let exists = |candidate: &str| self.root.join(candidate).is_file();
        let mut candidates = vec![path.clone()];
        for (javascript, typescript) in [
            (".js", [".ts", ".tsx"]),
            (".jsx", [".tsx", ".tsx"]),
            (".mjs", [".mts", ".mts"]),
            (".cjs", [".cts", ".cts"]),
        ] {
            if let Some(stem) = path.strip_suffix(javascript) {
                candidates.extend(typescript.iter().map(|ts| format!("{stem}{ts}")));
            }
        }
        candidates.extend(
            EXTENSIONS
                .iter()
                .map(|extension| format!("{path}{extension}")),
        );
        candidates.extend(
            EXTENSIONS
                .iter()
                .map(|extension| format!("{path}/index{extension}")),
        );
        match candidates.into_iter().find(|candidate| exists(candidate)) {
            Some(found) => Resolved::File(found),
            None => Resolved::Unknown,
        }
    }
}

enum Resolved {
    File(String),
    /// A package or a built-in: not the project's own code.
    Outside,
    Unknown,
}

const BUILTINS: [&str; 40] = [
    "assert",
    "async_hooks",
    "buffer",
    "child_process",
    "cluster",
    "console",
    "constants",
    "crypto",
    "dgram",
    "diagnostics_channel",
    "dns",
    "domain",
    "events",
    "fs",
    "http",
    "http2",
    "https",
    "inspector",
    "module",
    "net",
    "os",
    "path",
    "perf_hooks",
    "process",
    "punycode",
    "querystring",
    "readline",
    "repl",
    "stream",
    "string_decoder",
    "timers",
    "tls",
    "tty",
    "url",
    "util",
    "v8",
    "vm",
    "worker_threads",
    "zlib",
    "test",
];

fn package_name(specifier: &str) -> &str {
    let mut parts = specifier.splitn(3, '/');
    let first = parts.next().unwrap_or(specifier);
    if first.starts_with('@')
        && let Some(second) = parts.next()
    {
        return &specifier[..first.len() + 1 + second.len()];
    }
    first
}

/// `a/./b/../c` as `a/c`, refusing a path that climbs out of the root.
fn normal(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => {}
            Component::ParentDir => {
                parts.pop()?;
            }
            _ => return None,
        }
    }
    Some(parts.join("/"))
}

fn read_json(path: &Path) -> Option<Value> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&crate::source_discovery::strip_trailing_commas(
        &crate::source_discovery::strip_jsonc_comments(&text),
    ))
    .ok()
}

fn collect_strings(value: &Value, into: &mut Vec<String>) {
    match value {
        Value::String(text) => into.push(text.clone()),
        Value::Object(map) => map.values().for_each(|value| collect_strings(value, into)),
        Value::Array(items) => items.iter().for_each(|value| collect_strings(value, into)),
        _ => {}
    }
}

/// The root tsconfig's `paths`, through its `extends` chain, each target
/// relative to the root.
fn aliases(root: &Path) -> Vec<(String, Vec<PathBuf>)> {
    let mut found = Vec::new();
    let mut config = root.join("tsconfig.json");
    let mut seen = BTreeSet::new();
    while seen.insert(config.clone()) && seen.len() < 16 {
        let Some(json) = read_json(&config) else {
            break;
        };
        let directory = config.parent().unwrap_or(root).to_owned();
        let options = json.get("compilerOptions");
        let base = options
            .and_then(|options| options.get("baseUrl"))
            .and_then(Value::as_str)
            .map_or(directory.clone(), |base| directory.join(base));
        if found.is_empty()
            && let Some(paths) = options
                .and_then(|options| options.get("paths"))
                .and_then(Value::as_object)
        {
            for (pattern, targets) in paths {
                let targets = targets
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .filter_map(|target| {
                        base.join(target)
                            .strip_prefix(root)
                            .ok()
                            .map(Path::to_owned)
                    })
                    .collect();
                found.push((pattern.clone(), targets));
            }
        }
        match json.get("extends").and_then(Value::as_str) {
            Some(parent) if parent.starts_with('.') => {
                config = directory.join(parent);
                if config.extension().is_none() {
                    config.set_extension("json");
                }
            }
            _ => break,
        }
    }
    found
}

/// What a file imports: static imports and re-exports, `import()`, `require`
/// and the module mocks of Jest and Vitest, which name a module by its path.
/// Imports of types only are erased before anything runs.
fn parse(file: &str, source: &str) -> Option<Vec<Specifier>> {
    use oxc_ast::ast::*;
    use oxc_ast_visit::{Visit, walk};

    let source_type = crate::js_instrumenter::project_source_type(Path::new(file)).ok()?;
    let allocator = oxc_allocator::Allocator::default();
    let parsed = oxc_parser::Parser::new(&allocator, source, source_type).parse();
    if parsed.panicked {
        return None;
    }
    struct Collector {
        found: Vec<Specifier>,
    }
    fn literal(expression: &Expression<'_>) -> Specifier {
        match expression {
            Expression::StringLiteral(text) => Specifier::Literal(text.value.to_string()),
            Expression::TemplateLiteral(template) if template.expressions.is_empty() => template
                .quasis
                .first()
                .and_then(|quasi| quasi.value.cooked.as_ref())
                .map_or(Specifier::Computed, |text| {
                    Specifier::Literal(text.to_string())
                }),
            _ => Specifier::Computed,
        }
    }
    impl<'a> Visit<'a> for Collector {
        fn visit_import_declaration(&mut self, it: &ImportDeclaration<'a>) {
            if !it.import_kind.is_type() {
                self.found
                    .push(Specifier::Literal(it.source.value.to_string()));
            }
        }
        fn visit_export_all_declaration(&mut self, it: &ExportAllDeclaration<'a>) {
            if !it.export_kind.is_type() {
                self.found
                    .push(Specifier::Literal(it.source.value.to_string()));
            }
        }
        fn visit_export_named_declaration(&mut self, it: &ExportNamedDeclaration<'a>) {
            if let Some(source) = &it.source
                && !it.export_kind.is_type()
            {
                self.found
                    .push(Specifier::Literal(source.value.to_string()));
            }
            walk::walk_export_named_declaration(self, it);
        }
        fn visit_ts_import_equals_declaration(&mut self, it: &TSImportEqualsDeclaration<'a>) {
            if let TSModuleReference::ExternalModuleReference(reference) = &it.module_reference
                && !it.import_kind.is_type()
            {
                self.found
                    .push(Specifier::Literal(reference.expression.value.to_string()));
            }
        }
        fn visit_import_expression(&mut self, it: &ImportExpression<'a>) {
            self.found.push(literal(&it.source));
            walk::walk_import_expression(self, it);
        }
        fn visit_call_expression(&mut self, it: &CallExpression<'a>) {
            let loads = match &it.callee {
                Expression::Identifier(name) => name.name == "require",
                Expression::StaticMemberExpression(member) => {
                    let object = match &member.object {
                        Expression::Identifier(name) => name.name.as_str(),
                        _ => "",
                    };
                    let property = member.property.name.as_str();
                    matches!(
                        (object, property),
                        ("require", "resolve")
                            | ("jest", "mock" | "requireActual" | "doMock")
                            | ("vi", "mock" | "importActual" | "doMock")
                    )
                }
                _ => false,
            };
            if loads && let Some(first) = it.arguments.first() {
                self.found.push(match first.as_expression() {
                    Some(expression) => literal(expression),
                    None => Specifier::Computed,
                });
            }
            walk::walk_call_expression(self, it);
        }
    }
    let mut collector = Collector { found: Vec::new() };
    collector.visit_program(&parsed.program);
    Some(collector.found)
}

/// The test runner a configuration file belongs to, by its name. A change to
/// one runner's configuration changes nothing another runner does.
pub fn configuration_runner(file: &str) -> Option<&'static str> {
    let name = file.rsplit('/').next()?.to_ascii_lowercase();
    if !name.contains(".config.") && !name.contains(".workspace.") {
        return None;
    }
    let stem = name.split('.').next()?;
    match stem {
        "playwright" | "playwright-ct" => Some("playwright"),
        "vitest" => Some("vitest"),
        "jest" => Some("jest"),
        "ava" => Some("ava"),
        _ => None,
    }
}

fn strings(file: &str, source: &str) -> Vec<String> {
    use oxc_ast::ast::*;
    use oxc_ast_visit::Visit;
    let Ok(source_type) = crate::js_instrumenter::project_source_type(Path::new(file)) else {
        return Vec::new();
    };
    let allocator = oxc_allocator::Allocator::default();
    let parsed = oxc_parser::Parser::new(&allocator, source, source_type).parse();
    struct Collector(Vec<String>);
    impl<'a> Visit<'a> for Collector {
        fn visit_string_literal(&mut self, it: &StringLiteral<'a>) {
            self.0.push(it.value.to_string());
        }
    }
    let mut collector = Collector(Vec::new());
    collector.visit_program(&parsed.program);
    collector.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> PathBuf {
        static UNIQUE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-test-loads-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            UNIQUE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        for (file, text) in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        root
    }

    #[test]
    fn a_test_loads_its_helpers_and_not_another_suites() {
        let root = project(&[
            (
                "package.json",
                r#"{"devDependencies":{"@playwright/test":"1"}}"#,
            ),
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"baseUrl":".","paths":{"@/*":["src/*"]}}}"#,
            ),
            (
                "tests/e2e/helpers.ts",
                "import { x } from '@/lib/x';\nexport const get = () => x;\n",
            ),
            (
                "tests/e2e/app.spec.ts",
                "import { test } from '@playwright/test';\nimport { get } from './helpers';\nimport type { T } from './types';\n",
            ),
            ("tests/e2e/types.ts", "export type T = 1;\n"),
            (
                "tests/unit/a.test.ts",
                "import { y } from '../support/y.js';\n",
            ),
            ("tests/support/y.ts", "export const y = 1;\n"),
            ("src/lib/x.ts", "export const x = 1;\n"),
        ]);
        let imports = Imports::new(&root);
        let e2e = imports.loads(["tests/e2e/app.spec.ts"]);
        assert_eq!(e2e.open, None);
        assert!(e2e.files.contains("tests/e2e/helpers.ts"));
        assert!(e2e.files.contains("src/lib/x.ts"), "{e2e:?}");
        assert!(
            !e2e.files.contains("tests/e2e/types.ts"),
            "a type import loads nothing"
        );
        assert!(!e2e.files.contains("tests/support/y.ts"));
        let unit = imports.loads(["tests/unit/a.test.ts"]);
        assert!(unit.files.contains("tests/support/y.ts"), "{unit:?}");
        assert!(!unit.files.contains("tests/e2e/helpers.ts"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn what_cannot_be_followed_leaves_the_answer_open() {
        let root = project(&[
            ("package.json", "{}"),
            ("a.test.js", "const name = 'b';\nrequire(`./${name}`);\n"),
            ("c.test.js", "import 'unknown-alias/thing';\n"),
            (
                "d.test.js",
                "import fs from 'node:fs';\nimport path from 'path';\nimport './e.js';\n",
            ),
            ("e.js", "export {};\n"),
        ]);
        let imports = Imports::new(&root);
        assert!(imports.loads(["a.test.js"]).open.is_some());
        assert!(imports.loads(["c.test.js"]).open.is_some());
        let closed = imports.loads(["d.test.js"]);
        assert_eq!(closed.open, None);
        assert!(closed.files.contains("e.js"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_configuration_belongs_to_its_runner() {
        assert_eq!(
            configuration_runner("tests/e2e/playwright.config.ts"),
            Some("playwright")
        );
        assert_eq!(configuration_runner("vitest.config.mts"), Some("vitest"));
        assert_eq!(configuration_runner("jest.config.js"), Some("jest"));
        assert_eq!(
            configuration_runner("vite.config.ts"),
            None,
            "a build shared with the app"
        );
        assert_eq!(configuration_runner("tsconfig.json"), None);
        assert_eq!(configuration_runner("next.config.ts"), None);
    }

    #[test]
    fn a_configuration_names_its_setup_files() {
        let root = project(&[
            (
                "vitest.config.ts",
                "export default { test: { setupFiles: ['tests/unit/setup.ts'], include: ['tests/**/*.test.ts'] } };\n",
            ),
            ("tests/unit/setup.ts", "export {};\n"),
            (
                "e2e/playwright.config.ts",
                "export default { globalSetup: './global-setup', testMatch: '*.spec.ts' };\n",
            ),
            ("e2e/global-setup.ts", "export default () => {};\n"),
        ]);
        let imports = Imports::new(&root);
        assert_eq!(
            imports.named_by("vitest.config.ts"),
            ["tests/unit/setup.ts"]
        );
        assert_eq!(
            imports.named_by("e2e/playwright.config.ts"),
            ["e2e/global-setup.ts"]
        );
        fs::remove_dir_all(root).unwrap();
    }
}
