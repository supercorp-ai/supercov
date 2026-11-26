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
    [".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"]
        .iter()
        .any(|extension| file.ends_with(extension))
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
fn nodes(code: &Code, source: &str) -> Vec<Node> {
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
            Node {
                name: own_name(&unit.path),
                parent: unit.parent,
                names,
                bare,
                always: top && (unit.kind == "method" || !declared(&source[..start])),
                inert: unit.inert,
            }
        })
        .collect()
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
        // Comments name nothing that runs.
        let nodes = nodes(
            &now,
            &crate::source_units::javascript_without_comments(file, &source)
                .expect("`code` parsed it"),
        );
        let modules = module_bindings(&source);
        let exports_module = source.contains("export * as ");
        if same {
            return File::Read {
                nodes,
                changed: BTreeSet::new(),
                modules,
                exports_module,
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
        }
    }

    /// Whether a test of `entry` that loads `files` could run, with a mutant
    /// switched on, code that changed since the earlier run. Read once per
    /// test file.
    pub fn reaches_change(&self, entry: &str, files: &BTreeSet<String>) -> bool {
        if let Some(reaches) = self.reaches.borrow().get(entry) {
            return *reaches;
        }
        let reaches = self.reach(entry, files);
        self.reaches.borrow_mut().insert(entry.to_owned(), reaches);
        reaches
    }

    fn reach(&self, entry: &str, files: &BTreeSet<String>) -> bool {
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
                    ..
                } => {
                    changes |= !changed.is_empty();
                    exported_whole |= *exports_module;
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
                    let all = *file == entry;
                    (0..nodes.len()).map(|index| all || index == 0).collect()
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
                                && (parent != 0
                                    || node.always
                                    || node
                                        .name
                                        .as_deref()
                                        .is_none_or(|name| named.contains(name)));
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
