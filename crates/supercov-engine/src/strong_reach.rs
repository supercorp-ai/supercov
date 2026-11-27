//! Whether a test with a mutant switched on could run code that changed
//! since an earlier run (`docs/design/strong-mutation.md`, "After an edit").
//!
//! A test's record says what it ran without a mutant. With one, it may run
//! what it never ran: the mutant changes a value, a branch goes the other
//! way, and a function the test never called is called. An earlier verdict
//! of that mutant holds only where no such function changed since.
//!
//! The bound is what the test's process loads and what that code can name.
//! A JavaScript or TypeScript test runs the files its imports load
//! (`test_loads`, every import followed), and code there runs only where it
//! is reached: a module's top level as it loads, a declaration nested in
//! code that runs, and a top-level declaration only where some code that
//! runs names it. So the declarations the test can run are found by names,
//! from the test file and every top level, until nothing new is named. A
//! name is any identifier in the code, strings included (comments are not
//! code), so a declaration named alike anywhere counts: the bound only
//! grows. What a
//! name cannot follow gives up the bound: a module object read other than
//! by a property name (`import * as all` read as `all[name]`, or passed on),
//! a default export, a method of an object literal at the top level, a
//! declaration whose name is not an identifier. A Python or Ruby file, whose
//! code is reached in more ways than its names say (decorators that register
//! a function, `send`), is reached whole where its code changed.
//!
//! A Rust test runs in a test binary that links every crate it depends on,
//! so every Rust file counts, and code there runs only where it is reached
//! by name, as above, with what Rust calls unnamed (`docs/design/rust-mutation.md`,
//! "After an edit"): a method of a trait implementation runs wherever its
//! type or its trait is named (an operator, `Drop`, `Display` through
//! `format!`, `Iterator` through `for`), not only its own name; a
//! declaration an attribute may hand to code that never names it
//! (`#[no_mangle]`, a procedural macro's attribute) runs wherever what
//! holds it does; and a file whose macros build names (`paste!`'s `[<..>]`,
//! `concat_idents!`) gives the bound up.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::rc::Rc;

use crate::source_manifest::{FileFingerprint, FileManifest};
use crate::source_units::Code;

/// One declaration, as reaching reads it.
#[derive(Debug)]
struct Node {
    /// What code names it by: `None` where a name cannot reach it alone
    /// (its own name is no identifier).
    name: Option<String>,
    parent: Option<usize>,
    /// Every identifier in its own text, its nested declarations' aside.
    names: BTreeSet<String>,
    /// Identifiers its own text reads other than before a `.`.
    bare: BTreeSet<String>,
    /// Reached with its parent whatever names it.
    always: bool,
    /// No runtime presence: a type.
    inert: bool,
    /// Reached only by a name even where its parent is reached: a Rust
    /// item of a module, an implementation or a trait.
    free: bool,
    /// Other names that reach it: the trait and the type of the Rust trait
    /// implementation it is a method of.
    keys: BTreeSet<String>,
    /// Its name path in the file (`tests::it_works`).
    path: String,
}

/// One file of what a test loads, as it is now against the earlier run.
#[derive(Debug)]
enum File {
    /// Its declarations, those that changed (none where it is as it was),
    /// and the bindings of whole modules it makes.
    Read {
        nodes: Vec<Node>,
        changed: BTreeSet<usize>,
        modules: BTreeSet<String>,
        /// It exports a whole module as one value (`export * as all`),
        /// which its importers read under a name of their own.
        exports_module: bool,
        /// Its macros build names no text holds (`paste!`, `concat_idents!`).
        builds_names: bool,
    },
    /// Its code changed in a way only running it whole can bound: its top
    /// level, a Python or Ruby file, a file no parser reads.
    Whole,
    /// Unchanged, and holding no code reaching reads: data, styles.
    Inert,
}

/// The code of a checkout against an earlier run's record of it.
pub struct Program {
    root: PathBuf,
    recorded: FileManifest,
    files: RefCell<BTreeMap<String, Rc<File>>>,
    reaches: RefCell<BTreeMap<String, bool>>,
}

/// Whether `file` holds code reaching reads by names.
fn by_names(file: &str) -> bool {
    [
        ".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx", ".rs",
    ]
    .iter()
    .any(|extension| file.ends_with(extension))
}

/// Whether `file` is Rust.
fn rust(file: &str) -> bool {
    file.ends_with(".rs")
}

/// The attributes that never hand what they sit on to code that does not
/// name it: what they do is the compiler's, or a derive's, whose code calls
/// the methods of the types the item names.
const PLAIN_ATTRIBUTES: &[&str] = &[
    "allow",
    "automatically_derived",
    "cold",
    "deny",
    "deprecated",
    "derive",
    "diagnostic",
    "doc",
    "expect",
    "forbid",
    "ignore",
    "inline",
    "must_use",
    "non_exhaustive",
    "repr",
    "rustfmt",
    "clippy",
    "should_panic",
    "test",
    "track_caller",
    "warn",
    "cfg",
];

/// The paths of the attributes in a Rust item's own text (`#[a::b(..)]`
/// gives `a::b`), inner ones (`#![..]`) included.
fn rust_attributes(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(at) = rest.find('#') {
        rest = &rest[at + 1..];
        let after = rest.trim_start_matches('!').trim_start();
        let Some(inside) = after.strip_prefix('[') else {
            continue;
        };
        let path = inside
            .trim_start()
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
            .collect::<String>();
        found.push(path);
    }
    found
}

/// The names that reach a method of the Rust implementation whose header
/// (`impl<T: Bound> Trait for Type<T> where ..`) is `header`: the trait's
/// and the type's, where it implements a trait; none for an inherent one.
fn implementation_keys(header: &str) -> BTreeSet<String> {
    let header = header
        .trim_start()
        .strip_prefix("unsafe")
        .unwrap_or(header)
        .trim_start();
    let Some(rest) = header.strip_prefix("impl") else {
        return BTreeSet::new();
    };
    // The generics after `impl`, and their parameters' names, which name
    // no type of the program.
    let mut depth = 0usize;
    let mut parameters = BTreeSet::new();
    let mut part = String::new();
    let mut end = 0;
    for (at, c) in rest.char_indices() {
        if depth == 0 && !c.is_whitespace() && c != '<' {
            break;
        }
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            _ => {}
        }
        if depth == 1 && matches!(c, '<' | ',') || depth == 0 {
            let name = part
                .trim()
                .trim_start_matches("const ")
                .trim_start_matches('\'')
                .split(|c: char| !word(c))
                .next();
            parameters.extend(name.filter(|name| !name.is_empty()).map(str::to_owned));
            part.clear();
        } else {
            part.push(c);
        }
        end = at + c.len_utf8();
    }
    let rest = &rest[end..];
    let rest = rest.split(" where ").next().unwrap_or(rest);
    if !rest.contains(" for ") {
        return BTreeSet::new();
    }
    identifiers(rest)
        .0
        .into_iter()
        .filter(|name| {
            !["for", "dyn", "mut", "const", "impl"].contains(&name.as_str())
                && !parameters.contains(name)
        })
        .collect()
}

/// Whether `c` can be part of an identifier.
fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// Each identifier of `text`, and those read other than before a `.`.
fn identifiers(text: &str) -> (BTreeSet<String>, BTreeSet<String>) {
    let mut names = BTreeSet::new();
    let mut bare = BTreeSet::new();
    let mut rest = text;
    while let Some(start) = rest.find(word) {
        let tail = &rest[start..];
        let length = tail.find(|c: char| !word(c)).unwrap_or(tail.len());
        let name = &tail[..length];
        rest = &tail[length..];
        // A number, not a name.
        if name.starts_with(|c: char| c.is_ascii_digit()) {
            continue;
        }
        names.insert(name.to_owned());
        let next = rest.trim_start();
        if !next.starts_with('.') || next.starts_with("...") {
            bare.insert(name.to_owned());
        }
    }
    (names, bare)
}

/// The bindings of whole modules in a file's text: `import * as all` and
/// `const all = require(...)`.
fn module_bindings(text: &str) -> BTreeSet<String> {
    let mut bindings = BTreeSet::new();
    let ident = |rest: &str| {
        rest.trim_start()
            .split(|c: char| !word(c))
            .next()
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
    };
    for (at, _) in text.match_indices("* as ") {
        bindings.extend(ident(&text[at + 5..]));
    }
    for (at, _) in text.match_indices("require(") {
        // `name = require(`, read back from the call.
        let before = text[..at].trim_end();
        if let Some(before) = before.strip_suffix('=') {
            let before = before.trim_end();
            let start = before.rfind(|c: char| !word(c)).map_or(0, |at| at + 1);
            if start < before.len() {
                bindings.insert(before[start..].to_owned());
            }
        }
    }
    bindings
}

/// `text` without the places whole modules are bound, which read the
/// binding's name without reading the module.
fn without_bindings(text: &str) -> String {
    let mut text = text.to_owned();
    for binding in module_bindings(&text) {
        text = text
            .replace(&format!("* as {binding}"), "* as ")
            .replace(&format!("{binding} = require("), "= require(");
    }
    text
}

/// A declaration's name as code names it: the last part of its path, where
/// that is an identifier.
fn own_name(path: &str) -> Option<String> {
    let last = path.rsplit(['.', ':']).next().unwrap_or_default();
    let last = last.split('#').next().unwrap_or_default();
    let identifier = !last.is_empty()
        && !last.starts_with(|c: char| c.is_ascii_digit())
        && last.chars().all(word);
    identifier.then(|| last.to_owned())
}

/// Whether a declaration that starts after `before` stands as a statement
/// of its own -- `function f`, `export class C` -- rather than as a value in
/// an expression: an argument (`app.get("/", function handle() {})`), a
/// value bound or assigned. A value runs where the code holding it puts it,
/// whatever names it.
fn declared(before: &str) -> bool {
    let before = before.trim_end();
    let last = before.rsplit(|c: char| !word(c)).next().unwrap_or_default();
    before.is_empty()
        || before.ends_with([';', '}', '{'])
        || ["export", "declare", "async"].contains(&last)
}

/// A file's declarations, as reaching reads them, from its source.
fn nodes(code: &Code, source: &str, rust: bool) -> Vec<Node> {
    let starts = crate::source_manifest::line_starts(source);
    let offset = |line: usize, column: usize| {
        starts
            .get(line - 1)
            .map_or(source.len(), |start| (start + column - 1).min(source.len()))
    };
    let spans = code
        .units
        .iter()
        .map(|unit| {
            (
                offset(unit.line, unit.column),
                offset(unit.end_line, unit.end_column),
            )
        })
        .collect::<Vec<_>>();
    let mut children = vec![Vec::new(); code.units.len()];
    code.units
        .iter()
        .enumerate()
        .filter_map(|(index, unit)| Some((unit.parent?, index)))
        .for_each(|(parent, index)| children[parent].push(index));
    code.units
        .iter()
        .enumerate()
        .map(|(index, unit)| {
            let (start, end) = spans[index];
            // Its own text: its children's spans left out.
            let mut own = String::new();
            let mut at = start;
            for child in &children[index] {
                let (child_start, child_end) = spans[*child];
                own.push_str(source.get(at..child_start.max(at)).unwrap_or_default());
                own.push(' ');
                at = child_end.max(at);
            }
            own.push_str(source.get(at..end.max(at)).unwrap_or_default());
            let own = without_bindings(&own);
            let (names, bare) = identifiers(&own);
            // A top-level declaration a name does not reach: an object's
            // method (an object exported whole, `export default {}`), or a
            // value an expression holds (`export default function`, an
            // argument).
            let top = unit.parent == Some(0);
            if rust {
                return rust_node(code, index, &own, names, bare);
            }
            Node {
                name: own_name(&unit.path),
                parent: unit.parent,
                names,
                bare,
                always: top && (unit.kind == "method" || !declared(&source[..start])),
                inert: unit.inert,
                free: false,
                keys: BTreeSet::new(),
                path: unit.path.clone(),
            }
        })
        .collect()
}

/// A Rust declaration, as reaching reads it. Modules, implementations and
/// traits hold items, and run nothing themselves: each is reached, and an
/// item in it only by its name, as an item of the file is. A function
/// nested in a function runs where that function does. A method of a trait
/// implementation is reached by its trait's or its type's name too, since
/// the language calls it unnamed. An item an attribute may hand to code that
/// never names it is reached with what holds it.
fn rust_node(
    code: &Code,
    index: usize,
    own: &str,
    names: BTreeSet<String>,
    bare: BTreeSet<String>,
) -> Node {
    let unit = &code.units[index];
    let holder = unit.parent.map(|parent| code.units[parent].kind.as_str());
    let container = matches!(unit.kind.as_str(), "mod" | "impl" | "trait");
    let attributes = rust_attributes(own);
    let unnamed = attributes.iter().any(|path| {
        let first = path.split("::").next().unwrap_or_default();
        !PLAIN_ATTRIBUTES.contains(&first)
    });
    // An implementation's or a trait's header runs nothing: the types and
    // traits it names are what reach its methods, not what it reaches.
    let names = if matches!(unit.kind.as_str(), "impl" | "trait") {
        BTreeSet::new()
    } else {
        names
    };
    let keys = match (holder, unit.parent) {
        (Some("impl"), Some(parent)) => {
            // The implementation's path is its header after the path of the
            // module it is in (`tests::impl<T> W<T>`).
            let path = &code.units[parent].path;
            let module = code.units[parent]
                .parent
                .map(|module| format!("{}::", code.units[module].path))
                .unwrap_or_default();
            implementation_keys(path.strip_prefix(module.as_str()).unwrap_or(path))
        }
        _ => BTreeSet::new(),
    };
    Node {
        name: own_name(&unit.path),
        parent: unit.parent,
        names,
        bare,
        always: index == 0 || container || unnamed,
        inert: unit.inert,
        free: holder.is_some_and(|holder| matches!(holder, "mod" | "impl" | "trait")),
        keys,
        path: unit.path.clone(),
    }
}

impl Program {
    pub fn new(root: PathBuf, recorded: FileManifest) -> Self {
        Self {
            root,
            recorded,
            files: RefCell::new(BTreeMap::new()),
            reaches: RefCell::new(BTreeMap::new()),
        }
    }

    /// The files the earlier run recorded.
    pub fn recorded_files(&self) -> Vec<String> {
        self.recorded.keys().cloned().collect()
    }

    /// One file now, against the earlier run's record: read once.
    fn file(&self, file: &str) -> Rc<File> {
        if let Some(read) = self.files.borrow().get(file) {
            return read.clone();
        }
        let read = Rc::new(self.read(file));
        self.files
            .borrow_mut()
            .insert(file.to_owned(), read.clone());
        read
    }

    fn read(&self, file: &str) -> File {
        let Ok(source) = crate::fault_fs::read_to_string(self.root.join(file)) else {
            return File::Whole;
        };
        // Set by the configuration and the dependencies, which are as they
        // were in a run whose verdicts are read at all.
        let settled = crate::run_currency::configuration_file(file)
            || crate::run_currency::dependency_file(file);
        let before = self.recorded.get(file);
        let same = settled
            || before.is_some_and(|before| before.same_bytes(&FileFingerprint::of(&source)));
        let now = by_names(file)
            .then(|| crate::source_units::code(file, &source))
            .flatten();
        let Some(now) = now else {
            return if same { File::Inert } else { File::Whole };
        };
        // Comments name nothing that runs. A Rust file's are kept: a name
        // they hold only grows what is reached.
        let nodes = if rust(file) {
            nodes(&now, &source, true)
        } else {
            nodes(
                &now,
                &crate::source_units::javascript_without_comments(file, &source)
                    .expect("`code` parsed it"),
                false,
            )
        };
        let (modules, exports_module, builds_names) = if rust(file) {
            (
                BTreeSet::new(),
                false,
                source.contains("[<") || source.contains("concat_idents!"),
            )
        } else {
            (
                module_bindings(&source),
                source.contains("export * as "),
                false,
            )
        };
        if same {
            return File::Read {
                nodes,
                changed: BTreeSet::new(),
                modules,
                exports_module,
                builds_names,
            };
        }
        let Some(before) = before.and_then(|before| before.code.as_ref()) else {
            return File::Whole;
        };
        if before.semantic == now.semantic {
            return File::Read {
                nodes,
                changed: BTreeSet::new(),
                modules,
                exports_module,
                builds_names,
            };
        }
        let diff = before.diff(&now);
        let index_now = now
            .units
            .iter()
            .enumerate()
            .map(|(index, unit)| (unit.path.as_str(), index))
            .collect::<BTreeMap<_, _>>();
        // Functions added alone change no code written before: the file's
        // top level holds one more placeholder.
        let added_only = crate::source_units::only_added(before, file, &source).is_some();
        let mut changed = diff
            .changed
            .iter()
            .filter(|index| !before.units[**index].inert)
            .map(|index| index_now[before.units[*index].path.as_str()])
            .chain(diff.added.iter().copied())
            .filter(|index| !now.units[*index].inert)
            .collect::<BTreeSet<_>>();
        if added_only {
            changed.remove(&0);
        }
        if changed.contains(&0) {
            return File::Whole;
        }
        File::Read {
            nodes,
            changed,
            modules,
            exports_module,
            builds_names,
        }
    }

    /// Whether a test of `entry` that loads `files` could run, with a mutant
    /// switched on, code that changed since the earlier run. Read once per
    /// test file.
    pub fn reaches_change(&self, entry: &str, files: &BTreeSet<String>) -> bool {
        self.reaches_change_from(entry, None, files)
    }

    /// [`Self::reaches_change`] for the test named `test` (its name path in
    /// `entry`, `tests::it_works`): a Rust test binary runs the test's own
    /// function, not every item of its file. Read once per test.
    pub fn reaches_change_from(
        &self,
        entry: &str,
        test: Option<&str>,
        files: &BTreeSet<String>,
    ) -> bool {
        let key = format!("{entry}\0{}", test.unwrap_or_default());
        if let Some(reaches) = self.reaches.borrow().get(&key) {
            return *reaches;
        }
        let reaches = self.reach(entry, test, files);
        self.reaches.borrow_mut().insert(key, reaches);
        reaches
    }

    fn reach(&self, entry: &str, test: Option<&str>, files: &BTreeSet<String>) -> bool {
        let read = files
            .iter()
            .map(|file| (file.as_str(), self.file(file)))
            .collect::<Vec<_>>();
        let mut changes = false;
        let mut exported_whole = false;
        for (_, file) in &read {
            match file.as_ref() {
                File::Whole => return true,
                File::Read {
                    changed,
                    exports_module,
                    builds_names,
                    ..
                } => {
                    changes |= !changed.is_empty();
                    // Names a macro builds are read by no one: as a module
                    // exported whole, the bound is given up.
                    exported_whole |= *exports_module || *builds_names;
                }
                File::Inert => {}
            }
        }
        // Nothing changed; or a module exported whole, read under names of
        // its importers' own.
        if !changes || exported_whole {
            return changes;
        }
        // Each file's declarations reached, and every name code reached
        // reads.
        let mut reached = read
            .iter()
            .map(|(file, read)| match read.as_ref() {
                File::Read { nodes, .. } => {
                    // A test whose own declaration is found runs from it
                    // (a Rust test); otherwise its whole file runs.
                    let own = test
                        .filter(|_| *file == entry)
                        .and_then(|test| nodes.iter().position(|node| node.path == test));
                    let all = *file == entry && own.is_none();
                    (0..nodes.len())
                        .map(|index| all || index == 0 || Some(index) == own)
                        .collect()
                }
                _ => Vec::new(),
            })
            .collect::<Vec<Vec<bool>>>();
        let mut named = BTreeSet::<&str>::new();
        let mut opaque = false;
        loop {
            let mut grew = false;
            read.iter()
                .zip(reached.iter_mut())
                .for_each(|((_, file), reached)| {
                    let File::Read { nodes, modules, .. } = file.as_ref() else {
                        return;
                    };
                    nodes.iter().enumerate().for_each(|(index, node)| {
                        if !reached[index] {
                            let parent = node.parent.unwrap_or_default();
                            // Nested in code that runs, or named by it.
                            let reachable = !node.inert
                                && reached[parent]
                                && ((parent != 0 && !node.free)
                                    || node.always
                                    || node
                                        .name
                                        .as_deref()
                                        .is_none_or(|name| named.contains(name))
                                    || node.keys.iter().any(|key| named.contains(key.as_str())));
                            if !reachable {
                                return;
                            }
                            reached[index] = true;
                            grew = true;
                        }
                        // A whole module read by more than its property
                        // names: a name no longer says what runs.
                        opaque |= node.bare.iter().any(|name| modules.contains(name));
                        let before = named.len();
                        named.extend(node.names.iter().map(String::as_str));
                        grew |= named.len() > before;
                    });
                });
            if opaque {
                return true;
            }
            if !grew {
                break;
            }
        }
        read.iter()
            .zip(&reached)
            .any(|((_, file), reached)| match file.as_ref() {
                File::Read { changed, .. } => changed.iter().any(|index| reached[*index]),
                _ => false,
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Checkout {
        root: PathBuf,
    }

    impl Checkout {
        fn new(label: &str, files: &[(&str, &str)]) -> Self {
            let root =
                std::env::temp_dir().join(format!("strong-reach-{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for (file, text) in files {
                let path = root.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, text).unwrap();
            }
            Self { root }
        }

        /// The manifest of the files as they are now: the earlier run's.
        fn recorded(&self, files: &[&str]) -> FileManifest {
            files
                .iter()
                .map(|file| {
                    let text = std::fs::read_to_string(self.root.join(file)).unwrap();
                    ((*file).to_owned(), FileFingerprint::read(file, &text))
                })
                .collect()
        }

        fn write(&self, file: &str, text: &str) {
            std::fs::write(self.root.join(file), text).unwrap();
        }

        fn reaches(&self, recorded: &FileManifest, entry: &str, files: &[&str]) -> bool {
            Program::new(self.root.clone(), recorded.clone()).reaches_change(
                entry,
                &files.iter().map(|file| (*file).to_owned()).collect(),
            )
        }
    }

    impl Drop for Checkout {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    const CART: &str = "import { audit } from \"./audit.js\";\nexport function refund(sum) {\n  if (sum > 1000) {\n    return audit(sum);\n  }\n  return sum - 1;\n}\n";
    const AUDIT: &str = "export function audit(sum) {\n  return sum;\n}\nexport function report(sum) {\n  return sum;\n}\n";
    const TEST: &str = "import { refund } from \"../src/cart.js\";\nit(\"refunds\", () => expect(refund(20)).toBe(19));\n";

    /// A function the test never ran unmutated but its code names is in
    /// reach; one nothing names is not, nor an unchanged checkout.
    #[test]
    fn a_change_is_reached_where_code_that_runs_names_it() {
        let checkout = Checkout::new(
            "named",
            &[
                ("src/cart.js", CART),
                ("src/audit.js", AUDIT),
                ("test/cart.test.js", TEST),
                ("vitest.config.js", "export default {};\n"),
                ("src/data.json", "{}\n"),
            ],
        );
        let files = [
            "test/cart.test.js",
            "src/cart.js",
            "src/audit.js",
            "vitest.config.js",
            "src/data.json",
        ];
        let recorded = checkout.recorded(&[
            "test/cart.test.js",
            "src/cart.js",
            "src/audit.js",
            "src/data.json",
        ]);
        assert!(!checkout.reaches(&recorded, "test/cart.test.js", &files));
        // A test file is read once: asked again, it answers as it did.
        let program = Program::new(checkout.root.clone(), recorded.clone());
        let loaded = files.iter().map(|file| (*file).to_owned()).collect();
        assert!(!program.reaches_change("test/cart.test.js", &loaded));
        checkout.write(
            "src/audit.js",
            &AUDIT.replacen("return sum;", "return -sum;", 1),
        );
        assert!(!program.reaches_change("test/cart.test.js", &loaded));
        // `audit` changed: `refund` names it.
        checkout.write(
            "src/audit.js",
            &AUDIT.replacen("return sum;", "return -sum;", 1),
        );
        assert!(checkout.reaches(&recorded, "test/cart.test.js", &files));
        // `report` changed: nothing that runs names it.
        checkout.write(
            "src/audit.js",
            &AUDIT
                .replace("  return sum;\n}\n", "  return 0;\n}\n")
                .replacen("return 0", "return sum", 1),
        );
        assert!(!checkout.reaches(&recorded, "test/cart.test.js", &files));
        // A comment, or a function added alone: nothing a program can tell.
        checkout.write("src/audit.js", &format!("// audit\n{AUDIT}"));
        assert!(!checkout.reaches(&recorded, "test/cart.test.js", &files));
        checkout.write(
            "src/audit.js",
            &format!("{AUDIT}export function later(sum) {{\n  return sum;\n}}\n"),
        );
        assert!(!checkout.reaches(&recorded, "test/cart.test.js", &files));
        // A top level that changed runs as the file loads.
        checkout.write("src/audit.js", &format!("export const rate = 2;\n{AUDIT}"));
        assert!(checkout.reaches(&recorded, "test/cart.test.js", &files));
        // A file the earlier run did not record, or that is gone.
        checkout.write("src/audit.js", AUDIT);
        assert!(checkout.reaches(&recorded, "test/cart.test.js", &["src/new.js"]));
        let unrecorded = checkout.recorded(&["src/cart.js"]);
        assert!(checkout.reaches(&unrecorded, "test/cart.test.js", &files));
    }

    const SHOP: &str = "pub fn refund(sum: u32) -> u32 {\n    if sum > 1000 {\n        audit(sum)\n    } else {\n        sum\n    }\n}\n\npub fn audit(sum: u32) -> u32 {\n    sum\n}\n\npub fn double(v: u32) -> u32 {\n    v * 2\n}\n\npub struct Money(pub u32);\n\nimpl std::fmt::Display for Money {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \"${}\", self.0)\n    }\n}\n\nimpl Money {\n    pub fn cents(&self) -> u32 {\n        self.0 * 100\n    }\n}\n\npub fn label(m: &Money) -> String {\n    format!(\"{m}\")\n}\n";
    const SHOP_TESTS: &str = "use shop::*;\n\n#[test]\nfn refunds() {\n    assert_eq!(refund(20), 20);\n}\n\n#[test]\nfn labels() {\n    assert_eq!(label(&Money(3)), \"$3\");\n}\n";

    /// A Rust test runs from its own function: what that names, and what
    /// that names in turn, with a trait implementation's methods reached by
    /// their type's name (`format!` calls `Display::fmt` unnamed), and an
    /// item an attribute may hand to other code reached with what holds it.
    #[test]
    fn a_rust_test_reaches_what_its_function_names() {
        let checkout = Checkout::new("rust", &[("src/lib.rs", SHOP), ("tests/t.rs", SHOP_TESTS)]);
        let files = ["src/lib.rs", "tests/t.rs"];
        let recorded = checkout.recorded(&files);
        let reaches = |test: &str| {
            Program::new(checkout.root.clone(), recorded.clone()).reaches_change_from(
                "tests/t.rs",
                Some(test),
                &files.iter().map(|file| (*file).to_owned()).collect(),
            )
        };
        assert!(!reaches("refunds"));
        // `audit`, which `refund` names.
        checkout.write(
            "src/lib.rs",
            &SHOP.replacen("    sum\n}", "    sum + 1\n}", 1),
        );
        assert!(reaches("refunds"));
        assert!(!reaches("labels"));
        // `double`: nothing either test runs names it.
        checkout.write("src/lib.rs", &SHOP.replace("v * 2", "2 * v"));
        assert!(!reaches("refunds"));
        assert!(!reaches("labels"));
        // `Display::fmt`, never named: `labels` names `Money`.
        checkout.write("src/lib.rs", &SHOP.replace("self.0)", "self.0 + 0)"));
        assert!(reaches("labels"));
        assert!(!reaches("refunds"));
        // An inherent method is reached by its name alone.
        checkout.write("src/lib.rs", &SHOP.replace("self.0 * 100", "100 * self.0"));
        assert!(!reaches("labels"));
        // A function exported unmangled may be called by code that never
        // names it.
        let exported = SHOP.replace("pub fn double", "#[no_mangle]\npub extern \"C\" fn double");
        checkout.write("src/lib.rs", &exported);
        let recorded_exported = checkout.recorded(&files);
        checkout.write("src/lib.rs", &exported.replace("v * 2", "2 * v"));
        assert!(
            Program::new(checkout.root.clone(), recorded_exported).reaches_change_from(
                "tests/t.rs",
                Some("refunds"),
                &files.iter().map(|file| (*file).to_owned()).collect(),
            )
        );
        // Names a macro builds give the bound up.
        let concatenated = format!("{SHOP}fn joined() {{ concat_idents!(dou, ble)(1); }}\n");
        checkout.write("src/lib.rs", &concatenated);
        let recorded_concatenated = checkout.recorded(&files);
        checkout.write("src/lib.rs", &concatenated.replace("v * 2", "2 * v"));
        assert!(
            Program::new(checkout.root.clone(), recorded_concatenated).reaches_change_from(
                "tests/t.rs",
                Some("refunds"),
                &files.iter().map(|file| (*file).to_owned()).collect(),
            )
        );
        let pasted = format!("{SHOP}paste::paste! {{ fn [<dou ble>]() {{}} }}\n");
        checkout.write("src/lib.rs", &pasted);
        let recorded_pasted = checkout.recorded(&files);
        checkout.write("src/lib.rs", &pasted.replace("v * 2", "2 * v"));
        assert!(
            Program::new(checkout.root.clone(), recorded_pasted).reaches_change_from(
                "tests/t.rs",
                Some("refunds"),
                &files.iter().map(|file| (*file).to_owned()).collect(),
            )
        );
        // A test whose function is not found runs its whole file.
        checkout.write("src/lib.rs", &SHOP.replace("v * 2", "2 * v"));
        assert!(!reaches("gone"));
        checkout.write("src/lib.rs", &SHOP.replace("self.0)", "self.0 + 0)"));
        assert!(reaches("gone"));
    }

    const GENERIC: &str = "pub mod inner {\n    pub struct W<T>(pub T);\n    impl<T: Copy> std::ops::Add for W<T>\n    where\n        T: std::ops::Add<Output = T>,\n    {\n        type Output = W<T>;\n        fn add(self, o: W<T>) -> W<T> {\n            W(self.0 + o.0)\n        }\n    }\n    pub struct P(pub *const u8);\n    unsafe impl Send for P {}\n    impl<T: Into<u8>> From<T> for P {\n        fn from(t: T) -> P {\n            P(std::ptr::null::<u8>().wrapping_add(t.into() as usize))\n        }\n    }\n    impl P {\n        pub fn get(&self) -> u8 {\n            1\n        }\n    }\n}\n\npub fn sum(a: u8, b: u8) -> u8 {\n    #![allow(unused)]\n    let tag = \"#x\";\n    (inner::W(a) + inner::W(b)).0\n}\n\n#[cfg_attr(test, inline)]\npub fn later() -> u8 {\n    2\n}\n";

    /// An operator implemented for a generic type in a module is reached by
    /// the type's name (`W`), whatever its header holds (generics, `where`);
    /// a method no code names is not; an item under an attribute the
    /// compiler does not own is reached with what holds it.
    #[test]
    fn a_rust_operator_of_a_generic_type_is_reached_by_its_type() {
        let tests = "use shop::*;\n\n#[test]\nfn sums() {\n    assert_eq!(sum(1, 2), 3);\n}\n";
        let checkout = Checkout::new(
            "rust-generic",
            &[("src/lib.rs", GENERIC), ("tests/t.rs", tests)],
        );
        let files = ["src/lib.rs", "tests/t.rs"];
        let recorded = checkout.recorded(&files);
        let reaches = || {
            Program::new(checkout.root.clone(), recorded.clone()).reaches_change_from(
                "tests/t.rs",
                Some("sums"),
                &files.iter().map(|file| (*file).to_owned()).collect(),
            )
        };
        assert!(!reaches());
        checkout.write(
            "src/lib.rs",
            &GENERIC.replace("self.0 + o.0", "o.0 + self.0"),
        );
        assert!(reaches());
        checkout.write(
            "src/lib.rs",
            &GENERIC.replace("            1\n", "            2\n"),
        );
        assert!(!reaches());
        checkout.write("src/lib.rs", &GENERIC.replace("    2\n}\n", "    3\n}\n"));
        assert!(reaches());
        assert_eq!(
            rust_attributes("#![allow(x)] # [inline] #x #[tokio::test]"),
            ["allow", "inline", "tokio::test"]
        );
        // `From::from` of `P`, whose header nests generics: `sum` never
        // names `P`, `From` or `from`.
        checkout.write(
            "src/lib.rs",
            &GENERIC.replace("t.into() as usize", "t.into() as usize + 0"),
        );
        assert!(!reaches());
        assert_eq!(
            implementation_keys("unsafe impl Send for P"),
            BTreeSet::from(["P".to_owned(), "Send".to_owned()])
        );
        assert!(implementation_keys("struct S").is_empty());
        assert!(implementation_keys("impl").is_empty());
        // A generic parameter names no type of the program.
        assert_eq!(
            implementation_keys("impl<'a, T: Into<u8>, const N: usize> From<[T; N]> for P<'a>"),
            BTreeSet::from(["From".to_owned(), "P".to_owned()])
        );
    }

    /// What a name cannot follow gives up the bound: a whole module read
    /// as a value, a default export, an object's method at the top level.
    #[test]
    fn what_names_cannot_follow_reaches_every_change() {
        let report_changed = AUDIT.replace(
            "export function report(sum) {\n  return sum;",
            "export function report(sum) {\n  return 0;",
        );
        for (test, audit, reached) in [
            (
                "import * as all from \"../src/audit.js\";\nit(\"a\", () => all[\"report\"](1));\n",
                report_changed.as_str(),
                true,
            ),
            (
                "import * as all from \"../src/audit.js\";\nit(\"a\", () => all.audit(1));\n",
                report_changed.as_str(),
                false,
            ),
            (
                "const all = require(\"../src/audit.js\");\nit(\"a\", () => Object.values(all));\n",
                report_changed.as_str(),
                true,
            ),
            (
                "import report from \"../src/audit.js\";\nit(\"a\", () => report(1));\n",
                "export default function quiet(sum) {\n  return 0;\n}\n",
                true,
            ),
            (
                "export * as all from \"../src/audit.js\";\nit(\"a\", () => 1);\n",
                report_changed.as_str(),
                true,
            ),
            (
                "import api from \"../src/audit.js\";\nit(\"a\", () => api[\"go\"]());\n",
                "export default {\n  go() {\n    return 0;\n  },\n};\n",
                true,
            ),
        ] {
            let checkout = Checkout::new(
                "unnamed",
                &[
                    ("test/a.test.js", test),
                    (
                        "src/audit.js",
                        if audit == report_changed {
                            AUDIT
                        } else {
                            "export default {\n  go() {\n    return 1;\n  },\n};\nexport default function quiet(sum) {\n  return 1;\n}\n"
                        },
                    ),
                ],
            );
            let files = ["test/a.test.js", "src/audit.js"];
            let recorded = checkout.recorded(&files);
            checkout.write("src/audit.js", audit);
            assert_eq!(
                checkout.reaches(&recorded, "test/a.test.js", &files),
                reached,
                "{test}"
            );
        }
    }

    /// A function a top-level expression holds -- an argument, a value
    /// bound -- runs where that expression puts it, named or not.
    #[test]
    fn a_function_held_by_an_expression_is_reached_unnamed() {
        let checkout = Checkout::new(
            "held",
            &[
                (
                    "test/a.test.js",
                    "import \"../src/app.js\";\nit(\"a\", () => request(\"/\"));\n",
                ),
                (
                    "src/app.js",
                    "app.get(\"/\",\n  function handle() {\n    return 1;\n  });\nexport function unused() {\n  return 1;\n}\n",
                ),
            ],
        );
        let files = ["test/a.test.js", "src/app.js"];
        let recorded = checkout.recorded(&files);
        let app = std::fs::read_to_string(checkout.root.join("src/app.js")).unwrap();
        checkout.write("src/app.js", &app.replacen("return 1", "return 2", 1));
        assert!(checkout.reaches(&recorded, "test/a.test.js", &files));
        checkout.write(
            "src/app.js",
            &app.replace("  return 1;\n}\n", "  return 2;\n}\n"),
        );
        assert!(!checkout.reaches(&recorded, "test/a.test.js", &files));
        assert!(declared("") && declared("x;\n") && declared("export ") && declared("}\n"));
        assert!(!declared("app.get(\"/\",\n  ") && !declared("const f = "));
    }

    /// A Python or Ruby file whose code changed, or a file no parser reads,
    /// is reached whole; one as it was is not.
    #[test]
    fn a_file_names_cannot_bound_is_reached_whole() {
        let checkout = Checkout::new(
            "whole",
            &[
                ("app.py", "def handler():\n    return 1\n"),
                ("notes.txt", "a\n"),
                ("test_app.py", "def test_it():\n    assert True\n"),
            ],
        );
        let files = ["test_app.py", "app.py", "notes.txt"];
        let recorded = checkout.recorded(&files);
        assert!(!checkout.reaches(&recorded, "test_app.py", &files));
        checkout.write("app.py", "def handler():\n    return 2\n");
        assert!(checkout.reaches(&recorded, "test_app.py", &files));
        checkout.write("app.py", "def handler():\n    return 1\n");
        checkout.write("notes.txt", "b\n");
        assert!(checkout.reaches(&recorded, "test_app.py", &files));
    }

    /// A method runs where its class does; a class, where a name reaches
    /// it; a type never.
    #[test]
    fn a_method_is_reached_with_its_class_and_a_type_never() {
        const SHAPES: &str = "export interface Shape {\n  area(): number;\n}\nexport class Square {\n  area() {\n    return 1;\n  }\n}\nexport class Unused {\n  area() {\n    return 0;\n  }\n}\n";
        let checkout = Checkout::new(
            "classes",
            &[
                (
                    "test/a.test.ts",
                    "import { Square } from \"../src/shapes.ts\";\nit(\"a\", () => new Square());\n",
                ),
                ("src/shapes.ts", SHAPES),
            ],
        );
        let files = ["test/a.test.ts", "src/shapes.ts"];
        let recorded = checkout.recorded(&files);
        // A class nothing names: its method does not run.
        checkout.write("src/shapes.ts", &SHAPES.replace("return 0", "return 2"));
        assert!(!checkout.reaches(&recorded, "test/a.test.ts", &files));
        // A type: nothing runs.
        checkout.write(
            "src/shapes.ts",
            &SHAPES.replace("area(): number", "area(): string"),
        );
        assert!(!checkout.reaches(&recorded, "test/a.test.ts", &files));
        // The class the test names: its method may run.
        checkout.write("src/shapes.ts", &SHAPES.replace("return 1", "return 2"));
        assert!(checkout.reaches(&recorded, "test/a.test.ts", &files));
        // Nothing loaded, nothing reached.
        let program = Program::new(checkout.root.clone(), recorded);
        assert!(!program.reaches_change("test/a.test.ts", &BTreeSet::new()));
    }

    #[test]
    fn names_and_bindings_are_read_from_text() {
        let (names, bare) = identifiers("a.b(c, ...d) + 1x $e _f a...g");
        let all = ["a", "b", "c", "d", "$e", "_f", "g"].map(str::to_owned);
        assert_eq!(names, all.clone().into());
        assert_eq!(bare, all.into());
        assert_eq!(identifiers(" + "), (BTreeSet::new(), BTreeSet::new()));
        assert_eq!(identifiers("x.y").1, ["y".to_owned()].into());
        assert_eq!(
            module_bindings(
                "import * as all from 'x';\nconst m = require('y');\nrequire('z');\nf(require('w'));\n= require('v');"
            ),
            ["all", "m"].map(str::to_owned).into()
        );
        assert_eq!(own_name("Server.start"), Some("start".into()));
        assert_eq!(own_name("tests::it#2"), Some("it".into()));
        assert_eq!(own_name("module.exports"), Some("exports".into()));
        assert_eq!(own_name("<<x>"), None);
        assert_eq!(own_name("9a"), None);
        assert_eq!(own_name("_a$"), Some("_a$".into()));
        assert_eq!(own_name(""), None);
    }
}
