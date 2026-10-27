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
use crate::fault_fs as fs;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, VecDeque},
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
    file.ends_with(".py")
        || file.ends_with(".rb")
        || EXTENSIONS[..8]
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
    python: RefCell<BTreeMap<String, Option<Vec<PythonImport>>>>,
    ruby: RefCell<BTreeMap<String, Option<Vec<RubyRequire>>>>,
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
            python: RefCell::new(BTreeMap::new()),
            ruby: RefCell::new(BTreeMap::new()),
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
            if file.ends_with(".py") {
                self.python_loads(&file, &mut loads, &mut queue);
                continue;
            }
            if file.ends_with(".rb") {
                self.ruby_loads(&file, &mut loads, &mut queue);
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

    /// What a Python file loads: the modules it imports, the packages they
    /// sit in (whose `__init__.py` runs first), pytest's `conftest.py` in its
    /// directory and every directory above, and the plugins they name. A
    /// module found nowhere in the project is the standard library's or an
    /// installed package's; a computed import, or a change to `sys.path`,
    /// leaves the answer open.
    fn python_loads(&self, file: &str, loads: &mut Loads, queue: &mut VecDeque<String>) {
        for directory in Path::new(file).ancestors().skip(1) {
            if let Some(conftest) = normal(&directory.join("conftest.py"))
                && self.root.join(&conftest).is_file()
            {
                queue.push_back(conftest);
            }
        }
        let known = self.python.borrow().get(file).cloned();
        let imports = known.unwrap_or_else(|| {
            let parsed = fs::read_to_string(self.root.join(file))
                .ok()
                .and_then(|source| python_imports(&source));
            self.python
                .borrow_mut()
                .insert(file.to_owned(), parsed.clone());
            parsed
        });
        let Some(imports) = imports else {
            loads
                .open
                .get_or_insert_with(|| format!("{file} could not be read"));
            return;
        };
        for import in imports {
            match import {
                PythonImport::Computed(why) => {
                    loads.open.get_or_insert_with(|| format!("{file} {why}"));
                }
                PythonImport::Module {
                    module,
                    level,
                    names,
                } => {
                    queue.extend(self.python_resolve(file, &module, level, &names));
                }
            }
        }
    }

    /// What a Ruby file loads: what it requires by a literal path, relative
    /// to it or on the load path a runner sets up (`lib`, `spec`, `test`),
    /// and, for a spec, what `.rspec` requires before every one. A feature
    /// found nowhere in the project is a gem's or the standard library's; a
    /// computed path, a glob of support files, or a change to `$LOAD_PATH`
    /// leaves the answer open.
    fn ruby_loads(&self, file: &str, loads: &mut Loads, queue: &mut VecDeque<String>) {
        let exists = |path: &Path| normal(path).filter(|path| self.root.join(path).is_file());
        let feature = |name: &str| {
            let name = name.strip_suffix(".rb").unwrap_or(name);
            ["lib", "spec", "test", ""]
                .iter()
                .find_map(|base| exists(&Path::new(base).join(format!("{name}.rb"))))
        };
        if file.ends_with("_spec.rb")
            && let Ok(options) = fs::read_to_string(self.root.join(".rspec"))
        {
            let words = options.split_whitespace().collect::<Vec<_>>();
            for pair in words.windows(2) {
                if matches!(pair[0], "--require" | "-r")
                    && let Some(required) = feature(pair[1])
                {
                    queue.push_back(required);
                }
            }
            for word in &words {
                if let Some(name) = word.strip_prefix("--require=")
                    && let Some(required) = feature(name)
                {
                    queue.push_back(required);
                }
            }
        }
        let known = self.ruby.borrow().get(file).cloned();
        let requires = known.unwrap_or_else(|| {
            let parsed = fs::read_to_string(self.root.join(file))
                .ok()
                .and_then(|source| ruby_requires(&source));
            self.ruby
                .borrow_mut()
                .insert(file.to_owned(), parsed.clone());
            parsed
        });
        let Some(requires) = requires else {
            loads
                .open
                .get_or_insert_with(|| format!("{file} could not be read"));
            return;
        };
        let directory = Path::new(file).parent().unwrap_or(Path::new(""));
        for required in requires {
            match required {
                RubyRequire::Relative(path) => {
                    let path = path.strip_suffix(".rb").unwrap_or(&path).to_owned();
                    match exists(&directory.join(format!("{path}.rb"))) {
                        Some(next) => queue.push_back(next),
                        None => {
                            loads.open.get_or_insert_with(|| {
                                format!("{file} requires '{path}', which Supercov cannot find")
                            });
                        }
                    }
                }
                RubyRequire::Feature(name) => {
                    if let Some(next) = feature(&name) {
                        queue.push_back(next);
                    }
                }
                RubyRequire::Computed(why) => {
                    loads.open.get_or_insert_with(|| format!("{file} {why}"));
                }
            }
        }
    }

    /// The project files an import runs, or none for a module outside it.
    fn python_resolve(
        &self,
        file: &str,
        module: &str,
        level: u32,
        names: &[String],
    ) -> Vec<String> {
        let exists = |path: &Path| normal(path).filter(|path| self.root.join(path).is_file());
        let parts = module
            .split('.')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>();
        let directory = Path::new(file).parent().unwrap_or(Path::new(""));
        let bases = if level > 0 {
            let mut base = directory.to_owned();
            for _ in 1..level {
                base = base.parent().unwrap_or(Path::new("")).to_owned();
            }
            vec![base]
        } else {
            // Where a test's imports are found: beside it and above (pytest
            // puts a test's root directory on the path), and a src layout.
            let mut bases = directory
                .ancestors()
                .map(Path::to_owned)
                .collect::<Vec<_>>();
            bases.push(PathBuf::from("src"));
            bases
        };
        for base in bases {
            let mut found = Vec::new();
            let mut package = base.clone();
            let mut resolved = parts.is_empty();
            for (index, part) in parts.iter().enumerate() {
                package = package.join(part);
                if let Some(init) = exists(&package.join("__init__.py")) {
                    found.push(init);
                    resolved = index + 1 == parts.len();
                } else if index + 1 == parts.len()
                    && let Some(module) = exists(&package.with_extension("py"))
                {
                    found.push(module);
                    resolved = true;
                } else {
                    break;
                }
            }
            if !resolved {
                continue;
            }
            // `from package import name` may name a submodule.
            for name in names {
                let submodule = package.join(name);
                if let Some(file) = exists(&submodule.with_extension("py"))
                    .or_else(|| exists(&submodule.join("__init__.py")))
                {
                    found.push(file);
                }
            }
            return found;
        }
        Vec::new()
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
        // A query or a fragment (`./icon.svg?raw`) names no other file. The
        // `#` that starts a subpath import is the name's own.
        let end = specifier
            .char_indices()
            .skip(1)
            .find(|(_, character)| matches!(character, '?' | '#'))
            .map_or(specifier.len(), |(index, _)| index);
        let specifier = &specifier[..end];
        if specifier.starts_with("./")
            || specifier.starts_with("../")
            || matches!(specifier, "." | "..")
        {
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
        // The root's own index where the path is the root.
        let directory = if path.is_empty() {
            String::new()
        } else {
            format!("{path}/")
        };
        candidates.extend(
            EXTENSIONS
                .iter()
                .map(|extension| format!("{directory}index{extension}")),
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
                // As TypeScript finds it: the path as written, or with
                // `.json` added where no file is there.
                config = directory.join(parent);
                if !config.is_file() && !parent.ends_with(".json") {
                    let mut named = config.into_os_string();
                    named.push(".json");
                    config = PathBuf::from(named);
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

#[derive(Debug, Clone)]
enum PythonImport {
    Module {
        module: String,
        /// Leading dots of a relative import.
        level: u32,
        names: Vec<String>,
    },
    /// Why what it loads cannot be read.
    Computed(&'static str),
}

/// Every import in a Python file, at any depth, with `importlib` calls that
/// name a module literally and pytest's `pytest_plugins`.
fn python_imports(source: &str) -> Option<Vec<PythonImport>> {
    use ruff_python_ast::{
        Expr, Stmt,
        visitor::{Visitor, walk_expr, walk_stmt},
    };
    let parsed = ruff_python_parser::parse_module(source).ok()?;
    struct Collector(Vec<PythonImport>);
    fn module(name: &str) -> PythonImport {
        PythonImport::Module {
            module: name.to_owned(),
            level: 0,
            names: Vec::new(),
        }
    }
    fn strings(expression: &Expr) -> Option<Vec<String>> {
        match expression {
            Expr::StringLiteral(text) => Some(vec![text.value.to_str().to_owned()]),
            Expr::List(list) => list
                .elts
                .iter()
                .map(|item| strings(item).map(|mut s| s.remove(0)))
                .collect(),
            Expr::Tuple(tuple) => tuple
                .elts
                .iter()
                .map(|item| strings(item).map(|mut s| s.remove(0)))
                .collect(),
            _ => None,
        }
    }
    fn is_sys_path(expression: &Expr) -> bool {
        matches!(expression, Expr::Attribute(attribute)
            if attribute.attr.as_str() == "path"
                && matches!(&*attribute.value, Expr::Name(name) if name.id.as_str() == "sys"))
    }
    impl<'a> Visitor<'a> for Collector {
        fn visit_stmt(&mut self, stmt: &'a Stmt) {
            match stmt {
                Stmt::Import(import) => {
                    for alias in &import.names {
                        self.0.push(module(alias.name.as_str()));
                    }
                }
                Stmt::ImportFrom(import) => self.0.push(PythonImport::Module {
                    module: import
                        .module
                        .as_ref()
                        .map(|m| m.to_string())
                        .unwrap_or_default(),
                    level: import.level,
                    names: import
                        .names
                        .iter()
                        .map(|alias| alias.name.to_string())
                        .collect(),
                }),
                Stmt::Assign(assign) => {
                    for target in &assign.targets {
                        if matches!(target, Expr::Name(name) if name.id.as_str() == "pytest_plugins")
                        {
                            match strings(&assign.value) {
                                Some(plugins) => self.0.extend(plugins.iter().map(|p| module(p))),
                                None => self.0.push(PythonImport::Computed(
                                    "names its pytest plugins by computation",
                                )),
                            }
                        }
                        if is_sys_path(target) {
                            self.0.push(PythonImport::Computed("changes sys.path"));
                        }
                    }
                }
                _ => {}
            }
            walk_stmt(self, stmt);
        }
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Call(call) = expr {
                let first = call.arguments.args.first();
                match &*call.func {
                    Expr::Attribute(attribute)
                        if is_sys_path(&attribute.value)
                            && matches!(
                                attribute.attr.as_str(),
                                "insert" | "append" | "extend"
                            ) =>
                    {
                        self.0.push(PythonImport::Computed("changes sys.path"));
                    }
                    Expr::Attribute(attribute) if attribute.attr.as_str() == "import_module" => {
                        self.0.push(match first.and_then(strings) {
                            Some(mut names) => module(&names.remove(0)),
                            None => PythonImport::Computed("imports a module named by computation"),
                        });
                    }
                    Expr::Name(name)
                        if matches!(name.id.as_str(), "__import__" | "import_module") =>
                    {
                        self.0.push(match first.and_then(strings) {
                            Some(mut names) => module(&names.remove(0)),
                            None => PythonImport::Computed("imports a module named by computation"),
                        });
                    }
                    _ => {}
                }
            }
            walk_expr(self, expr);
        }
    }
    let mut collector = Collector(Vec::new());
    collector.visit_body(&parsed.syntax().body);
    Some(collector.0)
}

/// The test runner a configuration file belongs to, by its name. A change to
/// one runner's configuration changes nothing another runner does.
#[derive(Debug, Clone)]
enum RubyRequire {
    /// `require_relative "x"`.
    Relative(String),
    /// `require "x"`, `load "x.rb"`, `autoload :X, "x"`: on the load path.
    Feature(String),
    Computed(&'static str),
}

/// What a Ruby file requires, or None when it does not parse.
fn ruby_requires(source: &str) -> Option<Vec<RubyRequire>> {
    use ruby_prism::{CallNode, GlobalVariableReadNode, Visit};
    let parsed = ruby_prism::parse(source.as_bytes());
    if parsed.errors().next().is_some() {
        return None;
    }
    struct Requires(Vec<RubyRequire>);
    impl<'pr> Visit<'pr> for Requires {
        fn visit_call_node(&mut self, node: &CallNode<'pr>) {
            let name = node.name();
            let name = name.as_slice();
            if node.receiver().is_none()
                && matches!(
                    name,
                    b"require" | b"require_relative" | b"load" | b"autoload"
                )
            {
                let arguments = node
                    .arguments()
                    .map(|arguments| arguments.arguments().iter().collect::<Vec<_>>())
                    .unwrap_or_default();
                // `autoload :Name, "path"` names its path second.
                let path = if name == b"autoload" {
                    arguments.get(1)
                } else {
                    arguments.first()
                };
                let literal = path
                    .and_then(|path| path.as_string_node())
                    .and_then(|path| String::from_utf8(path.unescaped().to_vec()).ok());
                self.0.push(match (literal, name) {
                    (Some(path), b"require_relative") => RubyRequire::Relative(path),
                    (Some(path), _) => RubyRequire::Feature(path),
                    (None, _) => RubyRequire::Computed("requires a computed path"),
                });
            }
            ruby_prism::visit_call_node(self, node);
        }
        fn visit_global_variable_read_node(&mut self, node: &GlobalVariableReadNode<'pr>) {
            if matches!(node.name().as_slice(), b"$LOAD_PATH" | b"$:") {
                self.0.push(RubyRequire::Computed("changes $LOAD_PATH"));
            }
        }
    }
    let mut requires = Requires(Vec::new());
    requires.visit(&parsed.node());
    Some(requires.0)
}

pub fn configuration_runner(file: &str) -> Option<&'static str> {
    let name = file
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !name.contains(".config.") && !name.contains(".workspace.") {
        return None;
    }
    let stem = name.split('.').next().unwrap_or_default();
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
    fn a_python_test_loads_its_conftest_and_what_it_imports() {
        let root = project(&[
            (
                "tests/conftest.py",
                "import pytest\nfrom tests.support import factory\n",
            ),
            ("tests/__init__.py", ""),
            ("tests/support/__init__.py", ""),
            ("tests/support/factory.py", "import json\n"),
            (
                "tests/unit/test_a.py",
                "from .helpers import make\nfrom shop import cart\n",
            ),
            ("tests/unit/__init__.py", ""),
            ("tests/unit/helpers.py", "def make():\n    return 1\n"),
            (
                "tests/e2e/test_b.py",
                "import requests\nfrom tests.e2e import pages\n",
            ),
            ("tests/e2e/__init__.py", ""),
            ("tests/e2e/pages.py", ""),
            ("src/shop/__init__.py", ""),
            ("src/shop/cart.py", ""),
            (
                "tests/odd/test_c.py",
                "import sys\nsys.path.insert(0, 'x')\n",
            ),
        ]);
        let imports = Imports::new(&root);
        let unit = imports.loads(["tests/unit/test_a.py"]);
        assert_eq!(unit.open, None, "{unit:?}");
        for file in [
            "tests/conftest.py",
            "tests/support/factory.py",
            "tests/unit/helpers.py",
            "src/shop/cart.py",
        ] {
            assert!(unit.files.contains(file), "{file}: {unit:?}");
        }
        assert!(!unit.files.contains("tests/e2e/pages.py"));
        let e2e = imports.loads(["tests/e2e/test_b.py"]);
        assert!(
            e2e.files.contains("tests/e2e/pages.py")
                && !e2e.files.contains("tests/unit/helpers.py"),
            "{e2e:?}"
        );
        assert!(imports.loads(["tests/odd/test_c.py"]).open.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_ruby_spec_loads_what_it_requires_and_what_rspec_requires() {
        let root = project(&[
            (".rspec", "--require spec_helper\n--format documentation\n"),
            (
                "spec/spec_helper.rb",
                "require \"json\"\nrequire_relative \"support/factories\"\n",
            ),
            ("spec/support/factories.rb", "module Factories; end\n"),
            ("spec/support/unused.rb", "module Unused; end\n"),
            (
                "spec/orders_spec.rb",
                "require \"orders\"\nRSpec.describe(Orders) { }\n",
            ),
            (
                "lib/orders.rb",
                "autoload :Refunds, \"orders/refunds\"\nmodule Orders; end\n",
            ),
            ("lib/orders/refunds.rb", "module Refunds; end\n"),
            (
                "spec/glob_spec.rb",
                "Dir[File.join(__dir__, \"support/*.rb\")].each { |f| require f }\n",
            ),
            ("spec/path_spec.rb", "$LOAD_PATH.unshift(\"vendor\")\n"),
        ]);
        let imports = Imports::new(&root);
        let loads = imports.loads(["spec/orders_spec.rb"]);
        assert_eq!(loads.open, None);
        assert_eq!(
            loads.files.into_iter().collect::<Vec<_>>(),
            [
                "lib/orders.rb",
                "lib/orders/refunds.rb",
                "spec/orders_spec.rb",
                "spec/spec_helper.rb",
                "spec/support/factories.rb",
            ]
        );
        assert!(imports.loads(["spec/glob_spec.rb"]).open.is_some());
        assert!(imports.loads(["spec/path_spec.rb"]).open.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    /// RSpec's `-r` and `--require=` load their files and a library name
    /// loads none; pytest_plugins loads its module; `require` and `export`
    /// in TypeScript load what they name unless only a type is taken; and
    /// only a change to the load path or `sys.path` leaves the answer open.
    #[test]
    fn each_runner_s_way_of_loading_a_file_is_followed() {
        let root = project(&[
            (
                ".rspec",
                "-r support/a\n--require=support/b\n--require json\n",
            ),
            ("spec/support/a.rb", ""),
            ("spec/support/b.rb", ""),
            ("spec/x_spec.rb", "RSpec.describe(1) { }\n"),
            ("spec/colon_spec.rb", "$:.unshift(\"vendor\")\n"),
            ("spec/global_spec.rb", "$stdout.sync = true\n"),
            (
                "tests/conftest.py",
                "pytest_plugins = [\"tests.plugins\"]\nother = 1\n",
            ),
            ("tests/__init__.py", ""),
            ("tests/plugins.py", ""),
            ("tests/test_a.py", "import os\n"),
            (
                "tests/test_path.py",
                "import sys\nsys.path.insert(0, \"x\")\n",
            ),
            ("tests/test_assign.py", "import sys\nsys.path = []\n"),
            (
                "tests/test_other.py",
                "import sys\nsys.argv.append(\"x\")\n",
            ),
            (
                "test/a.test.ts",
                "import helper = require(\"./helper\");\nimport type T = require(\"./types\");\nexport type { X } from \"./x\";\nexport { y } from \"./y\";\nrequire();\n",
            ),
            ("test/helper.ts", ""),
            ("test/types.ts", ""),
            ("test/x.ts", ""),
            ("test/y.ts", ""),
        ]);
        let imports = Imports::new(&root);
        let loaded = |file: &str| {
            let loads = imports.loads([file]);
            (loads.files.into_iter().collect::<Vec<_>>(), loads.open)
        };
        assert_eq!(
            loaded("spec/x_spec.rb"),
            (
                vec![
                    "spec/support/a.rb".to_owned(),
                    "spec/support/b.rb".into(),
                    "spec/x_spec.rb".into()
                ],
                None
            )
        );
        assert_eq!(
            loaded("spec/colon_spec.rb").1.as_deref(),
            Some("spec/colon_spec.rb changes $LOAD_PATH")
        );
        assert_eq!(loaded("spec/global_spec.rb").1, None);
        assert_eq!(
            loaded("tests/test_a.py"),
            (
                vec![
                    "tests/__init__.py".to_owned(),
                    "tests/conftest.py".into(),
                    "tests/plugins.py".into(),
                    "tests/test_a.py".into(),
                ],
                None
            )
        );
        for changes in ["tests/test_path.py", "tests/test_assign.py"] {
            assert_eq!(
                loaded(changes).1,
                Some(format!("{changes} changes sys.path")),
                "{changes}"
            );
        }
        assert_eq!(loaded("tests/test_other.py").1, None);
        assert_eq!(
            loaded("test/a.test.ts"),
            (
                vec![
                    "test/a.test.ts".to_owned(),
                    "test/helper.ts".into(),
                    "test/y.ts".into()
                ],
                None
            )
        );
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

    /// Every way a JavaScript, Python or Ruby test names what it loads:
    /// followed where Supercov can, and the first it cannot named.
    #[test]
    fn each_specifier_shape_is_followed_or_named() {
        let root = project(&[
            (
                "package.json",
                r##"{"name":"app","imports":{"#lib/*":"./src/lib/*.js","#util":"./src/util.js","#pkg":"dep"},"dependencies":{"@scope/pkg":"1","dep":"1"}}"##,
            ),
            ("tsconfig.json", r#"{"extends":"./tsconfig.base.json"}"#),
            (
                "tsconfig.base.json",
                r#"{"compilerOptions":{"paths":{"~/*":["src/*"]}}}"#,
            ),
            ("src/lib/x.js", "export default 1;\n"),
            ("src/util.js", "export default 1;\n"),
            ("src/y.ts", "export default 1;\n"),
            ("tests/types.ts", "export type T = 1;\n"),
            ("tests/z.ts", "export const z = 1;\n"),
            ("tests/star.ts", "export const s = 1;\n"),
            ("tests/cjs.ts", "export = 1;\n"),
            ("tests/resolved.js", "module.exports = 1;\n"),
            ("tests/mocked.js", "module.exports = 1;\n"),
            ("tests/literal.js", "module.exports = 1;\n"),
            (
                "tests/a.test.ts",
                "import x from '#lib/x';\nimport u from '#util';\nimport y from '~/y';\nimport s from '@scope/pkg';\nimport d from 'dep/sub';\nexport type { T } from './types';\nexport { z } from './z';\nexport * from './star';\nimport fs = require('./cjs');\nrequire.resolve('./resolved');\njest.mock('./mocked');\nrequire(`./literal`);\n",
            ),
            ("tests/abs.test.js", "import a from '/abs/file';\n"),
            ("tests/hash.test.js", "import a from '#missing';\n"),
            (
                "tests/package.test.js",
                "import a from '#pkg';\nimport b from './z.ts#fragment';\n",
            ),
            ("tests/scope.test.js", "import a from '@scope';\n"),
            (
                "tests/nm.test.js",
                "import a from './node_modules/x/index.js';\n",
            ),
            (
                "tests/test_p.py",
                "import sys\nimport importlib\npytest_plugins = [\"tests.plugin\"]\nsys.path.insert(0, \"src\")\nimportlib.import_module(\"x\")\n__import__(\"y\")\n",
            ),
            ("tests/plugin.py", "X = 1\n"),
            (".rspec", "--require spec_helper\n--require=other\n"),
            ("spec/spec_helper.rb", "X = 1\n"),
            ("spec/other.rb", "Y = 1\n"),
            (
                "spec/a_spec.rb",
                "$LOAD_PATH.unshift(__dir__)\nrequire_relative 'other'\n",
            ),
        ]);
        let imports = Imports::new(&root);
        let all = imports.loads(["tests/a.test.ts"]);
        assert_eq!(all.open, None, "{all:?}");
        for file in [
            "src/lib/x.js",
            "src/util.js",
            "src/y.ts",
            "tests/z.ts",
            "tests/star.ts",
            "tests/cjs.ts",
            "tests/resolved.js",
            "tests/mocked.js",
            "tests/literal.js",
        ] {
            assert!(all.files.contains(file), "{file}: {all:?}");
        }
        assert!(
            !all.files.contains("tests/types.ts"),
            "a type export loads nothing"
        );
        let packaged = imports.loads(["tests/package.test.js"]);
        assert_eq!(packaged.open, None, "{packaged:?}");
        assert!(packaged.files.contains("tests/z.ts"));
        for (entry, open) in [
            ("tests/abs.test.js", "imports '/abs/file'"),
            ("tests/hash.test.js", "imports '#missing'"),
            ("tests/scope.test.js", "imports '@scope'"),
            ("tests/test_p.py", "changes sys.path"),
            ("spec/a_spec.rb", "changes $LOAD_PATH"),
        ] {
            let loads = imports.loads([entry]);
            assert!(
                loads
                    .open
                    .as_deref()
                    .is_some_and(|said| said.contains(open)),
                "{entry}: {loads:?}"
            );
        }
        assert_eq!(imports.loads(["tests/nm.test.js"]).open, None);
        let ruby = imports.loads(["spec/a_spec.rb"]);
        assert!(
            ruby.files.contains("spec/spec_helper.rb") && ruby.files.contains("spec/other.rb"),
            "{ruby:?}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_setup_file_named_twice_is_named_once() {
        let root = project(&[
            (
                "vitest.config.ts",
                "export default { test: { setupFiles: ['./setup.ts', './setup.ts'] } };\n",
            ),
            ("setup.ts", "export {};\n"),
        ]);
        let imports = Imports::new(&root);
        assert_eq!(imports.named_by("vitest.config.ts"), ["setup.ts"]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_workspace_file_belongs_to_its_runner() {
        assert_eq!(configuration_runner("vitest.workspace.ts"), Some("vitest"));
        assert_eq!(configuration_runner("vitest.setup.ts"), None);
    }

    #[test]
    fn data_a_test_imports_is_loaded_and_not_read_for_imports() {
        let root = project(&[
            ("package.json", "{}"),
            (
                "a.test.js",
                "import data from './data.json';\nimport notes from './notes.txt?raw#top';\n",
            ),
            ("data.json", "{\"a\": 1}\n"),
            ("notes.txt", "notes\n"),
        ]);
        let loads = Imports::new(&root).loads(["a.test.js"]);
        assert_eq!(loads.open, None, "{loads:?}");
        assert_eq!(
            loads.files.into_iter().collect::<Vec<_>>(),
            ["a.test.js", "data.json", "notes.txt"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_test_that_imports_its_package_directory_loads_its_index() {
        let root = project(&[
            ("package.json", "{}"),
            ("a.test.js", "import main from '.';\n"),
            ("test/b.test.js", "const main = require('..');\n"),
            ("index.js", "module.exports = 1;\n"),
        ]);
        let imports = Imports::new(&root);
        for test in ["a.test.js", "test/b.test.js"] {
            let loads = imports.loads([test]);
            assert_eq!(loads.open, None, "{loads:?}");
            assert!(loads.files.contains("index.js"), "{loads:?}");
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn type_only_exports_and_namespace_aliases_load_nothing() {
        let root = project(&[
            ("package.json", "{}"),
            (
                "a.test.ts",
                "export type * from './types';\nimport Alias = Space.Inner;\nimport helper = require('./helper');\n",
            ),
            ("types.ts", "export type T = 1;\n"),
            ("helper.ts", "export = 1;\n"),
        ]);
        let loads = Imports::new(&root).loads(["a.test.ts"]);
        assert_eq!(loads.open, None, "{loads:?}");
        assert_eq!(
            loads.files.into_iter().collect::<Vec<_>>(),
            ["a.test.ts", "helper.ts"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_that_does_not_parse_leaves_the_answer_open() {
        let root = project(&[
            ("package.json", "{}"),
            ("a.test.js", "import { from './x';\n"),
            ("b_spec.rb", "def broken(\n"),
        ]);
        let imports = Imports::new(&root);
        assert_eq!(
            imports.loads(["a.test.js"]).open.as_deref(),
            Some("a.test.js could not be read")
        );
        assert_eq!(
            imports.loads(["b_spec.rb"]).open.as_deref(),
            Some("b_spec.rb could not be read")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_alias_or_subpath_falls_through_to_a_target_that_exists() {
        let root = project(&[
            (
                "package.json",
                r##"{"imports":{"#missing":"./gone.js","#fallback":["./gone.js","./lib/real.js"]}}"##,
            ),
            (
                "tsconfig.json",
                r#"{"compilerOptions":{"paths":{"@/*":["./generated/*","./src/*"]}}}"#,
            ),
            ("src/x.ts", "export const x = 1;\n"),
            ("lib/real.js", "export {};\n"),
            (
                "a.test.ts",
                "import { x } from '@/x';\nimport '#fallback';\n",
            ),
            ("b.test.ts", "import '#missing';\n"),
        ]);
        let imports = Imports::new(&root);
        let loads = imports.loads(["a.test.ts"]);
        assert_eq!(loads.open, None, "{loads:?}");
        assert!(loads.files.contains("src/x.ts"), "{loads:?}");
        assert!(loads.files.contains("lib/real.js"), "{loads:?}");
        assert!(imports.loads(["b.test.ts"]).open.is_some());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tsconfig_paths_come_from_the_nearest_config_that_sets_them() {
        let root = project(&[
            (
                "tsconfig.json",
                r#"{"extends":"./tsconfig.base.json","compilerOptions":{"paths":{"@app/*":["src/*"]}}}"#,
            ),
            (
                "tsconfig.base.json",
                r#"{"extends":"@tsconfig/node20/tsconfig.json","compilerOptions":{"paths":{"@app/*":["old/*"],"@base/*":["base/*"]}}}"#,
            ),
        ]);
        assert_eq!(
            aliases(&root),
            [(String::from("@app/*"), vec![PathBuf::from("src/*")])]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_tsconfig_extends_a_config_named_without_json_as_typescript_does() {
        // TypeScript reads the path as written and, where no file is there,
        // with `.json` added: `./tsconfig.base` is `tsconfig.base.json`.
        let root = project(&[
            ("tsconfig.json", r#"{"extends":"./tsconfig.base"}"#),
            ("tsconfig.base.json", r#"{"extends":"./configs/common"}"#),
            (
                "configs/common.json",
                r#"{"compilerOptions":{"paths":{"@common/*":["*"]}}}"#,
            ),
        ]);
        assert_eq!(
            aliases(&root),
            [(String::from("@common/*"), vec![PathBuf::from("configs/*")])]
        );
        fs::remove_dir_all(root).unwrap();
        let root = project(&[(
            "tsconfig.json",
            r#"{"extends":"./gone.json","compilerOptions":{"paths":{"@/*":["*"]}}}"#,
        )]);
        assert_eq!(
            aliases(&root).len(),
            1,
            "a config that is gone ends the chain"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_tsconfig_extends_chain_that_loops_or_runs_long_ends() {
        let root = project(&[
            (
                "tsconfig.json",
                r#"{"extends":"./b.json","compilerOptions":{}}"#,
            ),
            (
                "b.json",
                r#"{"extends":"./tsconfig.json","compilerOptions":{"paths":{"@b/*":["b/*"]}}}"#,
            ),
        ]);
        assert_eq!(aliases(&root).len(), 1);
        fs::remove_dir_all(root).unwrap();
        // Each config extends the next; the sixteenth is never read.
        let mut files = (0..20)
            .map(|index| {
                let name = if index == 0 {
                    String::from("tsconfig.json")
                } else {
                    format!("c{index}.json")
                };
                let paths = if index == 15 {
                    r#","compilerOptions":{"paths":{"@far/*":["far/*"]}}"#
                } else {
                    ""
                };
                (
                    name,
                    format!(r#"{{"extends":"./c{}.json"{paths}}}"#, index + 1),
                )
            })
            .collect::<Vec<_>>();
        files.push((String::from("c20.json"), String::from("{}")));
        let files = files
            .iter()
            .map(|(name, text)| (name.as_str(), text.as_str()))
            .collect::<Vec<_>>();
        let root = project(&files);
        assert_eq!(aliases(&root), []);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_spec_without_rspec_options_loads_what_it_requires() {
        let root = project(&[
            ("spec/a_spec.rb", "require_relative \"helper\"\n"),
            ("spec/helper.rb", "module Helper; end\n"),
        ]);
        let loads = Imports::new(&root).loads(["spec/a_spec.rb"]);
        assert_eq!(loads.open, None);
        assert_eq!(
            loads.files.into_iter().collect::<Vec<_>>(),
            ["spec/a_spec.rb", "spec/helper.rb"]
        );
        fs::remove_dir_all(root).unwrap();
        let root = project(&[
            (".rspec", "--require=rails_helper --require=spec_helper\n"),
            ("spec/spec_helper.rb", "\n"),
            ("spec/a_spec.rb", "\n"),
        ]);
        let loads = Imports::new(&root).loads(["spec/a_spec.rb"]);
        assert_eq!(
            loads.files.into_iter().collect::<Vec<_>>(),
            ["spec/a_spec.rb", "spec/spec_helper.rb"],
            "a gem's helper is not the project's"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn reading_sys_path_leaves_a_python_answer_closed() {
        let root = project(&[(
            "test_a.py",
            "import sys\nif sys.path.count('x'):\n    pass\n",
        )]);
        let loads = Imports::new(&root).loads(["test_a.py"]);
        assert_eq!(loads.open, None, "{loads:?}");
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_python_test_outside_the_checkout_cannot_be_read() {
        let root = project(&[("tests/test_a.py", "\n")]);
        let loads = Imports::new(&root.join("tests")).loads(["../elsewhere/test_a.py"]);
        assert_eq!(
            loads.files.into_iter().collect::<Vec<_>>(),
            ["../elsewhere/test_a.py"]
        );
        assert!(loads.open.is_some());
        fs::remove_dir_all(root).unwrap();
    }
}
