//! A mutant's anchor: what it is by its code rather than by its lines
//! (`docs/design/strong-mutation.md`, "After an edit").
//!
//! A mutant's id holds its span, so a line added above a function gives each
//! of its mutants a new id though none of them changed. Its anchor holds
//! instead the text of the innermost declaration it sits in (`source_units`),
//! byte for byte and comments included, its name path, where in that text the
//! mutant is, and what it does: two mutants with the same anchor change the
//! same code the same way, wherever their declaration moved. Any change to
//! the declaration's own lines, a comment or a space, gives each of its
//! mutants a new anchor, and an earlier verdict is then not found: anchors
//! only ever lose a match, never make a wrong one.
//!
//! A pass saves each verdict's anchor beside it, computed from the files it
//! ran against, and the next pass finds an earlier verdict by the anchor its
//! mutant has now.

use std::collections::BTreeMap;
use std::path::Path;

use sha2::{Digest, Sha256};

/// A mutant's span, as its id holds it.
struct Span {
    start: (usize, usize),
    end: (usize, usize),
    /// What follows the span: its mutator and descriptor.
    what: String,
}

/// The span and the rest of a mutant's id
/// (`file:startLine:startCol-endLine:endCol:Mutator:descriptor`).
fn span(id: &str, file: &str) -> Option<Span> {
    let rest = id.strip_prefix(file)?.strip_prefix(':')?;
    let (start_line, rest) = rest.split_once(':')?;
    let (start_column, rest) = rest.split_once('-')?;
    let (end_line, rest) = rest.split_once(':')?;
    let (end_column, what) = rest.split_once(':')?;
    Some(Span {
        start: (start_line.parse().ok()?, start_column.parse().ok()?),
        end: (end_line.parse().ok()?, end_column.parse().ok()?),
        what: what.to_owned(),
    })
}

/// The anchor of each mutant of one file's source, by id: `mutants` are
/// their ids. A mutant whose id has no span has none.
pub fn of_source<'a>(
    file: &str,
    source: &str,
    mutants: impl IntoIterator<Item = &'a str>,
) -> BTreeMap<String, String> {
    let lines = source.split_inclusive('\n').collect::<Vec<_>>();
    // A file no parser reads is one declaration, the file.
    let units = crate::source_units::code(file, source)
        .map(|code| {
            code.units
                .iter()
                .enumerate()
                .map(|(index, unit)| {
                    (
                        unit.path.clone(),
                        unit.line,
                        unit.end_line,
                        code.ancestors(index).count(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_else(|| vec![(String::new(), 1, lines.len().max(1), 1)]);
    let mut anchors = BTreeMap::new();
    for id in mutants {
        let Some(Span {
            start: (start_line, start_column),
            end: (end_line, end_column),
            what,
        }) = span(id, file)
        else {
            continue;
        };
        // The innermost declaration whose lines hold the mutant's: the
        // deepest, the first of those as deep.
        let Some((path, first, last, _)) = units
            .iter()
            .filter(|(_, first, last, _)| *first <= start_line && end_line <= *last)
            .fold(
                None,
                |innermost: Option<&(String, usize, usize, usize)>, unit| match innermost {
                    Some(held) if held.3 >= unit.3 => Some(held),
                    _ => Some(unit),
                },
            )
        else {
            continue;
        };
        let text = lines
            .get(first - 1..(*last).min(lines.len()))
            .unwrap_or_default()
            .concat();
        let mut digest = Sha256::new();
        for part in [
            file,
            path,
            &text,
            &format!(
                "{}:{start_column}-{}:{end_column}",
                start_line - first,
                end_line - first
            ),
            &what,
        ] {
            digest.update(part.len().to_le_bytes());
            digest.update(part.as_bytes());
        }
        let digest = digest.finalize();
        anchors.insert(
            id.to_owned(),
            digest[..8].iter().map(|b| format!("{b:02x}")).collect(),
        );
    }
    anchors
}

/// The anchor of each mutant whose file is under `root` as `unchanged` says
/// (its bytes are those the mutant was listed from), by id: `mutants` are
/// each mutant's id and file.
pub fn of_files<'a>(
    root: &Path,
    mutants: impl IntoIterator<Item = (&'a str, &'a str)>,
    unchanged: impl Fn(&str, &str) -> bool,
) -> BTreeMap<String, String> {
    let mut by_file = BTreeMap::<&str, Vec<&str>>::new();
    for (id, file) in mutants {
        by_file.entry(file).or_default().push(id);
    }
    let mut anchors = BTreeMap::new();
    for (file, ids) in by_file {
        let Ok(source) = crate::fault_fs::read_to_string(root.join(file)) else {
            continue;
        };
        if !unchanged(file, &source) {
            continue;
        }
        anchors.extend(of_source(file, &source, ids));
    }
    anchors
}

#[cfg(test)]
mod tests {
    use super::*;

    const CART: &str = "export function shipping(sum) {\n  if (sum >= 50) {\n    return 0;\n  }\n  return 5;\n}\n\nexport function label(name) {\n  return name.trim();\n}\n";

    fn anchor(source: &str, id: &str) -> Option<String> {
        of_source("src/cart.js", source, [id]).remove(id)
    }

    #[test]
    fn a_mutant_keeps_its_anchor_where_its_declaration_moved_whole() {
        let before = anchor(CART, "src/cart.js:2:7-2:16:EqualityOperator:>").unwrap();
        // Two lines above it: its id moves, its anchor does not.
        let moved = format!("// cart\n\n{CART}");
        assert_eq!(
            anchor(&moved, "src/cart.js:4:7-4:16:EqualityOperator:>"),
            Some(before.clone())
        );
        // Another function changed: its anchor is its own.
        let other = CART.replace("name.trim()", "name.trim().toUpperCase()");
        assert_eq!(
            anchor(&other, "src/cart.js:2:7-2:16:EqualityOperator:>"),
            Some(before.clone())
        );
        // A comment in its own declaration, another mutator at its place, or
        // another place: another anchor.
        let commented = CART.replace("    return 0;", "    // free\n    return 0;");
        for (source, id) in [
            (&commented, "src/cart.js:2:7-2:16:EqualityOperator:>"),
            (&CART.to_owned(), "src/cart.js:2:7-2:16:EqualityOperator:<"),
            (&CART.to_owned(), "src/cart.js:2:8-2:16:EqualityOperator:>"),
        ] {
            assert_ne!(anchor(source, id), Some(before.clone()), "{id}");
        }
        // The same text in another declaration is another mutant.
        let twice = format!(
            "{CART}export function free(sum) {{\n  if (sum >= 50) {{\n    return 0;\n  }}\n  return 5;\n}}\n"
        );
        assert_ne!(
            anchor(&twice, "src/cart.js:12:7-12:16:EqualityOperator:>"),
            Some(before)
        );
    }

    #[test]
    fn an_id_with_no_span_or_a_file_no_parser_reads() {
        assert_eq!(anchor(CART, "src/cart.js:Mutator"), None);
        assert_eq!(anchor(CART, "src/other.js:2:7-2:16:M:x"), None);
        assert_eq!(anchor(CART, "src/cart.js:x:7-2:16:M:x"), None);
        for broken in [
            "src/cart.jsx:2:7-2:16:M:x",
            "src/cart.js:2:7",
            "src/cart.js:2:7-2",
            "src/cart.js:2:7-2:16",
            "src/cart.js:2:x-2:16:M:x",
            "src/cart.js:2:7-y:16:M:x",
            "src/cart.js:2:7-2:z:M:x",
        ] {
            assert_eq!(anchor(CART, broken), None, "{broken}");
        }
        assert!(of_source("src/cart.js", CART, []).is_empty());
        // Two declarations on one line: the first holds a mutant of either,
        // and its anchor is the line's.
        let one_line = "export function a() { return 1; } export function b() { return 2; }\n";
        let id = "src/cart.js:1:60-1:61:NumberLiteral:3";
        assert!(anchor(one_line, id).is_some());
        // Past the end of the file: held by no declaration.
        assert_eq!(anchor(CART, "src/cart.js:90:1-90:2:M:x"), None);
        // A file no parser reads is one declaration: any change moves it.
        let text = "a = 1\nb = 2\n";
        let first = of_source("notes.txt", text, ["notes.txt:2:5-2:6:M:x"]);
        let again = of_source("notes.txt", text, ["notes.txt:2:5-2:6:M:x"]);
        assert_eq!(first, again);
        assert_eq!(first.len(), 1);
        let edited = of_source("notes.txt", "a = 2\nb = 2\n", ["notes.txt:2:5-2:6:M:x"]);
        assert_ne!(first, edited);
    }

    #[test]
    fn anchors_are_read_only_from_files_as_they_were_listed() {
        let root = std::env::temp_dir().join(format!("anchors-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/cart.js"), CART).unwrap();
        let id = "src/cart.js:2:7-2:16:EqualityOperator:>";
        let mutants = [
            (id, "src/cart.js"),
            ("src/gone.js:1:1-1:2:M:x", "src/gone.js"),
        ];
        let all = of_files(&root, mutants, |_, _| true);
        assert_eq!(all.keys().collect::<Vec<_>>(), [id]);
        assert!(of_files(&root, mutants, |_, _| false).is_empty());
        std::fs::remove_dir_all(root).unwrap();
    }
}
