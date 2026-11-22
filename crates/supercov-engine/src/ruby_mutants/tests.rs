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
    // A body with its clauses, written `begin ... end` where its own syntax
    // held it bare (a block's), is the same program.
    if node.ty == "kwbegin"
        && children.len() == 1
        && let Child::Node(only) = &children[0]
        && matches!(only.ty, "rescue" | "ensure")
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

/// Each mutant a value probe computes is linked to its kind at the probe's
/// site, and only where the probe records that kind.
#[test]
fn ruby_mutants_link_to_the_value_probes_that_compute_them() {
    let source = b"def m(a, s, b)\n  return 1 if a\n  t = s.strip\n  u = s.empty?\n  v = a != b\n  w = a + b.size\nend\n";
    let mut next = 0;
    let obligations =
        crate::ruby_instrumenter::build_ruby_obligations_with("a.rb", source, &mut next, true)
            .unwrap();
    let linked: std::collections::BTreeSet<(u32, String, String)> =
        candidates("a.rb", source, &obligations)
            .into_iter()
            .filter_map(|mutant| {
                let probe = mutant.probe?;
                Some((
                    mutant.start.0,
                    mutant.replacement,
                    probe.rsplit('#').next()?.to_owned(),
                ))
            })
            .collect();
    let expected = [
        (2, "true", "true"),
        (2, "false", "false"),
        (3, "s.lstrip", "lstrip"),
        (3, "s.rstrip", "rstrip"),
        (3, "s", "receiver"),
        (3, "nil", "nil"),
        (4, "s.any?", "any?"),
        (4, "true", "true"),
        (5, "(a == b)", "=="),
        (5, "(!a.eql?(b))", "not:eql?"),
        (5, "a", "left"),
        (5, "b", "right"),
        (6, "(a - b.size)", "-"),
        // Made `b.size` it leaves out `a`, a read; made `a` it would leave
        // out the call `b.size`, so that mutant is not linked.
        (6, "b.size", "right"),
    ];
    for (line, replacement, kind) in expected {
        assert!(
            linked.contains(&(line, replacement.to_owned(), kind.to_owned())),
            "{line}: {replacement} as {kind}: {linked:?}"
        );
    }
    assert!(
        !linked.contains(&(6, "a".to_owned(), "left".to_owned())),
        "{linked:?}"
    );
}

/// mutant's session results, plain or gzipped and from another checkout:
/// each evil mutation matched to Supercov's mutant by the method it makes,
/// with its verdict, and one no mutant makes kept as unmatched.
#[test]
fn ruby_mutant_sessions_join_by_the_method_each_mutation_makes() {
    use std::io::Write;
    let source = b"class Calc\n  def big?(n)\n    n > 10\n  end\nend\n";
    let result = |source: &str, kind: &str, criteria: serde_json::Value| {
        serde_json::json!({
            "mutation_result": {
                "mutation_type": kind,
                "mutation_source": source,
                "mutation_identification": format!("{kind}:Calc#big?:x"),
            },
            "criteria_result": criteria,
        })
    };
    let session = serde_json::json!({
        "subject_results": [
            { "identification": "Calc#big?:/elsewhere/project/lib/calc.rb:2" },
            {
                "source_path": "/elsewhere/project/lib/calc.rb",
                "identification": "Calc#big?:/elsewhere/project/lib/calc.rb:2",
                "coverage_results": [
                    result("def big?(n)\n  n == 10\nend", "evil", serde_json::json!({ "test_result": true })),
                    result("def big?(n)\n  n.eql?(10)\nend", "evil", serde_json::json!({ "process_abort": true })),
                    result("def big?(n)\n  nil\nend", "evil", serde_json::json!({ "timeout": true })),
                    result("def big?(n)\n  n > 11\nend", "evil", serde_json::json!({})),
                    result("def big?(n)\n  n.odd?\nend", "evil", serde_json::json!({})),
                    result("def big?(n)\n  n > 10\nend", "neutral", serde_json::json!({})),
                    result("def big?(n", "evil", serde_json::json!({})),
                ],
            },
        ],
    });
    let plain = serde_json::to_vec(&session).unwrap();
    let mut gzipped = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gzipped.write_all(&plain).unwrap();
    let gzipped = gzipped.finish().unwrap();
    assert!(is_mutant_session(&plain) && is_mutant_session(&gzipped));
    assert!(!is_mutant_session(b"{\"files\": {}}"));
    let read = |file: &str| (file == "lib/calc.rb").then(|| source.to_vec());
    for bytes in [&plain, &gzipped] {
        let joined = parse_mutant_session(bytes, |path| path.to_owned(), read).unwrap();
        let verdicts: Vec<(String, String)> = joined
            .iter()
            .map(|mutant| (mutant.id.clone().unwrap(), mutant.status.clone()))
            .collect();
        assert_eq!(
            verdicts,
            [
                ("lib/calc.rb:3:5-3:11:Send:n == 10", "Killed"),
                ("lib/calc.rb:3:5-3:11:Send:n.eql?(10)", "Killed"),
                ("lib/calc.rb:3:5-3:11:Send:nil", "Timeout"),
                ("lib/calc.rb:3:9-3:11:Integer:11", "Survived"),
                ("lib/calc.rb:mutant:evil:Calc#big?:x", "Survived"),
                ("lib/calc.rb:mutant:evil:Calc#big?:x", "Survived"),
            ]
            .map(|(id, status)| (id.to_owned(), status.to_owned()))
        );
        assert_eq!(joined[4].mutator, "Unmatched");
    }
    assert!(parse_mutant_session(b"{}", |path| path.to_owned(), read).is_err());
    assert!(parse_mutant_session(b"not json", |path| path.to_owned(), read).is_err());
    assert!(parse_mutant_session(&[0x1f, 0x8b, 0], |path| path.to_owned(), read).is_err());
}

/// A run's files listed again from their sources: a file whose points
/// changed since the run is named, one with no mutant left out.
#[test]
fn ruby_mutants_list_again_from_unchanged_files() {
    let root = std::env::temp_dir().join(format!("supercov-ruby-list-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rb"), "def a(x)\n  x + 1\nend\n").unwrap();
    fs::write(root.join("b.rb"), "def b(x)\n  x\nend\n").unwrap();
    fs::write(root.join("c.rb"), "# nothing\n").unwrap();
    let points = |file: &str| {
        let source = fs::read(root.join(file)).unwrap();
        let mut next = 0;
        crate::ruby_instrumenter::build_ruby_obligations(file, &source, &mut next)
            .unwrap()
            .manifest
            .points
            .iter()
            .map(|point| point.id.clone())
            .collect::<std::collections::BTreeSet<_>>()
    };
    let mut recorded = std::collections::BTreeMap::new();
    recorded.insert("a.rb".to_owned(), points("a.rb"));
    recorded.insert("b.rb".to_owned(), std::collections::BTreeSet::new());
    recorded.insert("c.rb".to_owned(), points("c.rb"));
    recorded.insert("gone.rb".to_owned(), std::collections::BTreeSet::new());
    let listed = list_mutants(&root, &recorded);
    assert_eq!(listed.changed, ["b.rb", "gone.rb"]);
    let ids = mutant_ids("a.rb", &fs::read(root.join("a.rb")).unwrap());
    assert!(
        ids.iter().any(|id| id == "a.rb:2:3-2:8:Send:x - 1"),
        "{ids:?}"
    );
    fs::remove_dir_all(&root).unwrap();
}

/// A file Prism cannot parse has no mutants; a method holding syntax the
/// port does not convert is skipped and named; two mutants that would
/// share an id are told apart by `#2`.
#[test]
fn ruby_enumeration_skips_what_it_cannot_read_and_keeps_ids_apart() {
    assert!(enumerate(b"def broken(\n").mutants.is_empty());
    let source = b"def complex\n  1.5i\nend\n\ndef fine(a)\n  a + 1\nend\n";
    let enumeration = enumerate(source);
    assert_eq!(enumeration.skipped.len(), 1, "{:?}", enumeration.skipped);
    assert!(enumeration.skipped[0].0.starts_with("complex"));
    assert!(!enumeration.mutants.is_empty());
    let ids = mutant_ids("a.rb", b"def twice(a)\n  [a, a]\nend\n");
    let unique: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(unique.len(), ids.len(), "{ids:?}");
}
