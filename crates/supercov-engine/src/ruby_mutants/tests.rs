use std::fs;
use std::path::Path;
use std::rc::Rc;

use serde_json::Value;

use super::tree::{Child, Converter, N, sexp};
use super::*;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/mutant-parity");

/// The parity fixtures, or the directory `MUTANT_PARITY_FIXTURES` names (a
/// project's files and their goldens, to compare on real code).
fn fixtures() -> Vec<std::path::PathBuf> {
    let directory = std::env::var("MUTANT_PARITY_FIXTURES").unwrap_or_else(|_| FIXTURES.to_owned());
    let mut paths: Vec<_> = fs::read_dir(directory)
        .expect("the parity fixtures")
        .map(|entry| entry.expect("a fixture").path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "rb"))
        .collect();
    paths.sort();
    paths
}

fn golden(path: &Path) -> Value {
    let name = path.file_name().unwrap().to_string_lossy();
    let golden = path.with_file_name(format!("{name}.mutant.json"));
    serde_json::from_str(&fs::read_to_string(&golden).expect("a golden")).expect("golden JSON")
}

fn defs(source: &[u8]) -> Vec<Rc<N>> {
    let result = ruby_prism::parse(source);
    let mut finder = Methods::default();
    finder.visit(&result.node());
    finder
        .defs
        .iter()
        .filter_map(|def| {
            Converter::new(source)
                .method(&def.as_def_node().unwrap())
                .ok()
        })
        .collect()
}

fn render(
    source: &[u8],
    start: usize,
    end: usize,
    node: &str,
    replacement: Option<String>,
) -> String {
    let (start_line, start_column) = position(source, start);
    let (end_line, end_column) = position(source, end);
    format!(
        "{start_line}:{start_column}-{end_line}:{end_column} {node} {}",
        replacement.unwrap_or_else(|| "removed".into())
    )
}

#[test]
fn ruby_methods_convert_as_parser_builds_them() {
    let mut failures = Vec::new();
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        let golden = golden(&path);
        let expected: Vec<String> = golden["subjects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|subject| subject["sexp"].as_str().unwrap().to_owned())
            .collect();
        let actual: Vec<String> = defs(&source).iter().map(|def| sexp(def)).collect();
        for (index, expected) in expected.iter().enumerate() {
            let actual = actual.get(index).map(String::as_str).unwrap_or("<missing>");
            if actual != expected {
                failures.push(format!(
                    "{} subject {index}:\n  mutant:   {expected}\n  supercov: {actual}",
                    path.display()
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn ruby_mutants_match_mutant_on_every_fixture() {
    let mut failures = Vec::new();
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        let golden = golden(&path);
        let mut expected = Vec::new();
        for subject in golden["subjects"].as_array().unwrap() {
            for mutant in subject["mutants"].as_array().unwrap() {
                let pair = |value: &Value| {
                    let pair = value.as_array().unwrap();
                    (pair[0].as_u64().unwrap(), pair[1].as_u64().unwrap())
                };
                let (start, end) = (pair(&mutant["start"]), pair(&mutant["end"]));
                expected.push(format!(
                    "{}:{}-{}:{} {} {}",
                    start.0,
                    start.1,
                    end.0,
                    end.1,
                    mutant["node"].as_str().unwrap(),
                    mutant["replacement"].as_str().unwrap_or("removed")
                ));
            }
        }
        let enumeration = enumerate(&source);
        let actual: Vec<String> = enumeration
            .mutants
            .iter()
            .map(|mutant| {
                render(
                    &source,
                    mutant.start,
                    mutant.end,
                    mutant.node,
                    mutant
                        .replacement
                        .as_ref()
                        .map(|replacement| sexp(replacement)),
                )
            })
            .collect();
        let count =
            |list: &[String], each: &String| list.iter().filter(|other| *other == each).count();
        let mut missing: Vec<&String> = expected
            .iter()
            .filter(|each| count(&actual, each) < count(&expected, each))
            .collect();
        let mut extra: Vec<&String> = actual
            .iter()
            .filter(|each| count(&expected, each) < count(&actual, each))
            .collect();
        missing.dedup();
        extra.dedup();
        if !missing.is_empty() || !extra.is_empty() || !enumeration.skipped.is_empty() {
            failures.push(format!(
                "{}: {} expected, {} enumerated, {} unprintable, skipped {:?}\n  missing:\n    {}\n  extra:\n    {}",
                path.display(),
                expected.len(),
                actual.len(),
                enumeration.unprintable,
                enumeration.skipped,
                missing.iter().map(|each| each.as_str()).collect::<Vec<_>>().join("\n    "),
                extra.iter().map(|each| each.as_str()).collect::<Vec<_>>().join("\n    ")
            ));
        } else if expected != actual {
            let first = expected
                .iter()
                .zip(&actual)
                .position(|(a, b)| a != b)
                .unwrap_or(0);
            failures.push(format!(
                "{}: same mutants in another order, from {first}:\n  mutant:   {:?}\n  supercov: {:?}",
                path.display(),
                &expected[first..(first + 4).min(expected.len())],
                &actual[first..(first + 4).min(actual.len())]
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// The tree as parsing printed code gives it back: parentheses around one
/// expression and `nil` for an empty body are the same program.
fn normalized(node: &N) -> Rc<N> {
    let children: Vec<Child> = node
        .children
        .iter()
        .map(|child| match child {
            Child::Node(inner) => {
                let inner = normalized(inner);
                if inner.ty == "nil"
                    && inner.children.is_empty()
                    && matches!(
                        node.ty,
                        "if" | "while"
                            | "until"
                            | "block"
                            | "def"
                            | "defs"
                            | "resbody"
                            | "in_pattern"
                            | "ensure"
                    )
                {
                    Child::Nil
                } else {
                    Child::Node(inner)
                }
            }
            other => other.clone(),
        })
        .collect();
    if node.ty == "begin"
        && children.len() == 1
        && let Child::Node(only) = &children[0]
    {
        return only.clone();
    }
    N::new(node.ty, children)
}

#[test]
fn printed_ruby_mutants_parse_back_to_their_trees() {
    let mut failures = Vec::new();
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        for def in defs(&source) {
            for mutated in mutators::mutate(&def, None) {
                if !validates(&mutated) {
                    continue;
                }
                let Some(Edit { start, end, text }) = edit(&source, &def, &mutated) else {
                    // A mutant on a line with a heredoc's opening has no code
                    // in place; its switch is placed around the heredoc.
                    let (from, to) = def.span.unwrap();
                    if !String::from_utf8_lossy(&source[from..to]).contains("<<~") {
                        failures.push(format!("unprintable: {}", sexp(&mutated)));
                    }
                    continue;
                };
                let mut spliced = source[..start].to_vec();
                spliced.extend_from_slice(text.as_bytes());
                spliced.extend_from_slice(&source[end..]);
                let result = ruby_prism::parse(&spliced);
                let errors: Vec<String> = result
                    .errors()
                    .map(|error| error.message().to_owned())
                    .collect();
                let reparsed = defs(&spliced)
                    .into_iter()
                    .find(|each| each.span.map(|span| span.0) == def.span.map(|span| span.0));
                let ok = errors.is_empty()
                    && reparsed
                        .as_ref()
                        .is_some_and(|reparsed| normalized(reparsed) == normalized(&mutated));
                if !ok {
                    failures.push(format!(
                        "{}: {} -> {:?}\n  errors: {errors:?}\n  want: {}\n  got:  {}",
                        path.display(),
                        String::from_utf8_lossy(&source[start..end]),
                        text,
                        sexp(&normalized(&mutated)),
                        reparsed
                            .map(|reparsed| sexp(&normalized(&reparsed)))
                            .unwrap_or_default()
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_fixture_mutant_has_a_switch_and_the_switched_file_parses_on_its_lines() {
    let mut failures = Vec::new();
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        let switches = schemata::switches(&source, 1, None);
        let switched = schemata::apply(&source, &switches.edits);
        let result = ruby_prism::parse(&switched);
        let errors: Vec<String> = result
            .errors()
            .map(|error| error.message().to_owned())
            .collect();
        let lines = |bytes: &[u8]| bytes.iter().filter(|byte| **byte == b'\n').count();
        if !errors.is_empty() || lines(&switched) != lines(&source) {
            failures.push(format!(
                "{}: {errors:?}\n{}",
                path.display(),
                String::from_utf8_lossy(&switched)
            ));
        }
        let unplaceable: Vec<_> = switches.unplaceable.iter().collect();
        if !unplaceable.is_empty() {
            let enumeration = enumerate(&source);
            for (index, reason) in unplaceable {
                let mutant = &enumeration.mutants[*index];
                failures.push(format!(
                    "{}: unplaceable ({reason}): {} -> {:?}",
                    path.display(),
                    String::from_utf8_lossy(&source[mutant.start..mutant.end]),
                    mutant.edit.text
                ));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
