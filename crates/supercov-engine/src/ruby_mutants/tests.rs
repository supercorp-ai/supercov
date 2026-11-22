use std::fs;
use std::path::Path;
use std::rc::Rc;

use serde_json::Value;

use super::tree::{Child, Converter, N, sexp, syntax};
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
                            | "when"
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
    // A string over several lines, printed on one, is one `str`: adjacent
    // strings are one string, and a `dstr` of strings alone a `str`.
    if node.ty == "dstr" {
        let mut merged: Vec<Child> = Vec::new();
        for child in children {
            let text = match &child {
                Child::Node(part) if part.ty == "str" => part.child(0).clone(),
                _ => {
                    merged.push(child);
                    continue;
                }
            };
            if let Some(Child::Node(last)) = merged.last()
                && last.ty == "str"
                && let (Child::Str(before), Child::Str(after)) = (last.child(0), &text)
            {
                let joined = [before.as_slice(), after.as_slice()].concat();
                *merged.last_mut().unwrap() = Child::Node(N::new("str", vec![Child::Str(joined)]));
                continue;
            }
            merged.push(Child::Node(N::new("str", vec![text])));
        }
        if let [Child::Node(only)] = merged.as_slice()
            && only.ty == "str"
        {
            return only.clone();
        }
        return N::new("dstr", merged);
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
    fs::write(root.join("d.rb"), "def broken(\n").unwrap();
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
    recorded.insert("d.rb".to_owned(), std::collections::BTreeSet::new());
    recorded.insert("gone.rb".to_owned(), std::collections::BTreeSet::new());
    let listed = list_mutants(&root, &recorded);
    assert_eq!(listed.changed, ["b.rb", "d.rb", "gone.rb"]);
    let nothing = list_mutants(&root, &std::collections::BTreeMap::new());
    assert!(nothing.changed.is_empty());
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

/// Every node of a method's Prism tree, by its byte range.
#[derive(Default)]
struct Spans {
    spans: Vec<(usize, usize)>,
    imaginary_floats: usize,
}

impl<'pr> ruby_prism::Visit<'pr> for Spans {
    fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        let location = node.location();
        self.spans
            .push((location.start_offset(), location.end_offset()));
    }

    fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
        let location = node.location();
        self.spans
            .push((location.start_offset(), location.end_offset()));
    }

    fn visit_imaginary_node(&mut self, node: &ruby_prism::ImaginaryNode<'pr>) {
        if node.numeric().as_float_node().is_some() {
            self.imaginary_floats += 1;
        }
        ruby_prism::visit_imaginary_node(self, node);
    }
}

/// A method holding syntax the port does not convert, wherever it is, is
/// skipped whole: no part of it is converted without the rest. Each node of
/// each fixture method is made `1.0i` (an imaginary float, which the port
/// refuses) in turn; where the method still parses and holds it, the method
/// is refused.
#[test]
fn a_method_holding_unconverted_syntax_anywhere_is_refused() {
    let mut failures = Vec::new();
    let mut refused = 0;
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        let result = ruby_prism::parse(&source);
        let mut finder = Methods::default();
        finder.visit(&result.node());
        for def in &finder.defs {
            let location = def.location();
            let text = &source[location.start_offset()..location.end_offset()];
            let parsed = ruby_prism::parse(text);
            let mut spans = Spans::default();
            spans.visit(&parsed.node());
            for (start, end) in spans.spans {
                let mut spliced = text[..start].to_vec();
                spliced.extend_from_slice(b"1.0i");
                spliced.extend_from_slice(&text[end..]);
                let reparsed = ruby_prism::parse(&spliced);
                if reparsed.errors().next().is_some() {
                    continue;
                }
                let mut holding = Spans::default();
                holding.visit(&reparsed.node());
                let mut methods = Methods::default();
                methods.visit(&reparsed.node());
                let Some(method) = methods.defs.first().and_then(|each| each.as_def_node()) else {
                    continue;
                };
                if holding.imaginary_floats == 0 {
                    continue;
                }
                match Converter::new(&spliced).method(&method) {
                    Err(_) => refused += 1,
                    Ok(tree) => failures.push(format!(
                        "{}: {} converted as {}",
                        path.display(),
                        String::from_utf8_lossy(&spliced),
                        sexp(&tree)
                    )),
                }
            }
        }
    }
    assert!(refused > 1000, "{refused}");
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// `node` with no span anywhere: printed from its children alone.
fn unspanned(node: &N) -> Rc<N> {
    Rc::new(N {
        ty: node.ty,
        children: node
            .children
            .iter()
            .map(|child| match child {
                Child::Node(inner) => Child::Node(unspanned(inner)),
                other => other.clone(),
            })
            .collect(),
        span: None,
        syntax: node.syntax,
    })
}

/// Every fixture method, and each of its mutants, printed from its
/// children alone, on one line, parses back to its own tree: each node's
/// printer, not only the source text it copies.
#[test]
fn ruby_methods_print_from_their_trees_and_parse_back() {
    let mut failures = Vec::new();
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        for method in defs(&source) {
            let mutated = mutators::mutate(&method, None)
                .into_iter()
                .filter(|mutated| validates(mutated));
            for def in std::iter::once(method.clone()).chain(mutated) {
                let bare = unspanned(&def);
                let Some(text) = print::one_line(&source, &method, &bare) else {
                    failures.push(format!("{}: unprintable {}", path.display(), sexp(&def)));
                    continue;
                };
                let reparsed = defs(text.as_bytes());
                let errors: Vec<String> = ruby_prism::parse(text.as_bytes())
                    .errors()
                    .map(|error| error.message().to_owned())
                    .collect();
                let same = reparsed
                    .first()
                    .is_some_and(|reparsed| normalized(reparsed) == normalized(&def));
                if !errors.is_empty() || !same {
                    failures.push(format!(
                        "{}: {text}\n  errors: {errors:?}\n  want: {}\n  got:  {}",
                        path.display(),
                        sexp(&normalized(&def)),
                        reparsed
                            .first()
                            .map(|reparsed| sexp(&normalized(reparsed)))
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

/// A node made another.
type Change<'a> = dyn Fn(&N) -> Rc<N> + 'a;

/// `tree` with the node at `target` (a preorder index) given to `change`.
fn changed_at(tree: &N, target: usize, next: &mut usize, change: &Change<'_>) -> Rc<N> {
    let index = *next;
    *next += 1;
    if index == target {
        return change(tree);
    }
    Rc::new(N {
        ty: tree.ty,
        children: tree
            .children
            .iter()
            .map(|child| match child {
                Child::Node(inner) => Child::Node(changed_at(inner, target, next, change)),
                other => other.clone(),
            })
            .collect(),
        span: tree.span,
        syntax: tree.syntax,
    })
}

fn preorder(tree: &Rc<N>, into: &mut Vec<Rc<N>>) {
    into.push(tree.clone());
    for child in tree.children.iter().filter_map(Child::node) {
        preorder(child, into);
    }
}

/// The printer, the mutators and the checks on a mutant never panic on a
/// tree they do not expect: each fixture method, printed from its
/// children, with one node made one the printer has no code for, or one
/// child of a node made `nil`, a symbol or a node where the other was, or
/// left out with those after it, also validated, located and edited as a
/// mutant of the method; and each such node mutated. Such trees are
/// refused, or handled as well as their parts allow.
#[test]
fn printing_and_mutating_never_panic_on_a_malformed_tree() {
    let unknown = |_: &N| N::new("unknown", Vec::new());
    let mut refused = 0;
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        for method in defs(&source) {
            let bare = unspanned(&method);
            let mut nodes = Vec::new();
            preorder(&bare, &mut nodes);
            for (target, node) in nodes.iter().enumerate() {
                let broken = changed_at(&bare, target, &mut 0, &unknown);
                if print::one_line(&source, &method, &broken).is_none() {
                    refused += 1;
                }
                let mut subtree = Vec::new();
                preorder(node, &mut subtree);
                let small = subtree.len() <= 12;
                for child in 0..node.children.len() {
                    let malformed: [&Change<'_>; 3] = [
                        &|node: &N| {
                            let mut children = node.children.clone();
                            children[child] = match &children[child] {
                                Child::Node(_) => Child::Nil,
                                _ => Child::Node(N::new("nil", Vec::new())),
                            };
                            node.with_children(children)
                        },
                        &|node: &N| {
                            let mut children = node.children.clone();
                            children[child] = Child::Sym("x".into());
                            node.with_children(children)
                        },
                        &|node: &N| {
                            let mut children = node.children.clone();
                            children.truncate(child);
                            node.with_children(children)
                        },
                    ];
                    for change in malformed {
                        let broken = changed_at(&bare, target, &mut 0, change);
                        let _ = print::one_line(&source, &method, &broken);
                        // As a mutant of the method: validated, located and
                        // edited.
                        let _ = validates(&broken);
                        let _ = localize(&method, &broken);
                        let _ = edit(&source, &method, &broken);
                        // Small nodes: each kind is small somewhere, and a
                        // node's mutations are its subtree's.
                        if small {
                            let _ = mutators::mutate(&change(node), None);
                        }
                    }
                }
            }
        }
    }
    assert!(refused > 1000, "{refused}");
}

/// Each kind of syntax the port does not convert yet skips its method with
/// the reason, and the methods around it keep their mutants.
#[test]
fn ruby_methods_with_unconverted_syntax_are_skipped_by_name() {
    let cases = [
        (
            "def m\n  \"a\\\n  b\"\nend\n",
            "a string with escaped line breaks",
        ),
        (
            "def m(a)\n  a =~ /a\n  b/\nend\n",
            "a regexp over several lines",
        ),
        (
            "def m(a)\n  a.select { (a == 1)..(a == 2) ? 1 : 2 }\nend\n",
            "a flip-flop",
        ),
        (
            "def m(a)\n  case a\n  in Point[x] then x\n  end\nend\n",
            "a constant array pattern",
        ),
        (
            "def m(a)\n  case a\n  in Point(x:) then x\n  end\nend\n",
            "a constant hash pattern",
        ),
        (
            "def m(a)\n  case a\n  in [x, ] then x\n  end\nend\n",
            "an implicit rest pattern",
        ),
        (
            "def m\n  class << self\n  end\nend\n",
            "syntax this port does not convert",
        ),
        ("def m\n  1.5i\nend\n", "an imaginary literal"),
        // The same refusals where they sit inside other syntax.
        (
            "def m(a)\n  /a\n  b/ =~ a\nend\n",
            "a regexp over several lines",
        ),
        (
            "def m(a)\n  /(?<x>a\n  b)/ =~ a\nend\n",
            "a regexp over several lines",
        ),
        (
            "def m(b)\n  \"a\\\n  #{b}\"\nend\n",
            "a string with escaped line breaks",
        ),
        (
            "def m(a)\n  (a.b(1.5i).c, d), e = a\nend\n",
            "an imaginary literal",
        ),
        (
            "def m(a)\n  b, *a.c(1.5i).d = a\nend\n",
            "an imaginary literal",
        ),
        (
            "def m(a)\n  *b, a.c(1.5i).d = a\nend\n",
            "an imaginary literal",
        ),
        (
            "def m(a)\n  (*a.c(1.5i).d), b = a\nend\n",
            "an imaginary literal",
        ),
        (
            "def m(a)\n  (b, *c), a.c(1.5i).d = a\nend\n",
            "an imaginary literal",
        ),
        (
            "def m(a)\n  ((b, a.c(1.5i).d)), e = a\nend\n",
            "an imaginary literal",
        ),
        (
            "def m(a)\n  for a.c(1.5i).d in a\n  end\nend\n",
            "an imaginary literal",
        ),
        (
            "def m\n  alias :\"a#{1.5i}\" b\nend\n",
            "an imaginary literal",
        ),
        (
            "def m\n  alias b :\"a#{1.5i}\"\nend\n",
            "an imaginary literal",
        ),
        (
            "def m\n  undef :\"a#{1.5i}\"\nend\n",
            "an imaginary literal",
        ),
    ];
    for (source, reason) in cases {
        let file = format!("{source}\ndef fine(a)\n  a + 1\nend\n");
        let enumeration = enumerate(file.as_bytes());
        assert_eq!(
            enumeration.skipped.len(),
            1,
            "{source}: {:?}",
            enumeration.skipped
        );
        assert!(
            enumeration.skipped[0].0.starts_with("m:1")
                && enumeration.skipped[0].1.contains(reason),
            "{source}: {:?}",
            enumeration.skipped
        );
        assert!(!enumeration.mutants.is_empty(), "{source}");
    }
}

/// Every statement and expression slot of every fixture method (where a
/// switch goes), printed from its children in its own place, leaves the
/// method parsing back to its own tree. Every other node printed in its
/// place, and other trees put in each place, are printed or refused, never
/// a panic.
#[test]
fn every_ruby_node_printed_in_its_own_place_parses_back() {
    let mut failures = Vec::new();
    let mut printed = 0;
    let others = [
        N::new("unknown", Vec::new()),
        N::new("str", vec![Child::Str(b"x".to_vec())]),
        N::new("sym", vec![Child::Sym("x".into())]),
        N::new("nil", Vec::new()),
        N::new("lvasgn", vec![Child::Sym("x".into())]),
        N::new("args", Vec::new()),
        N::new("regopt", Vec::new()),
        N::new(
            "if",
            vec![
                Child::Node(N::new("nil", Vec::new())),
                Child::Nil,
                Child::Nil,
            ],
        ),
    ];
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        for method in defs(&source) {
            let mut nodes = Vec::new();
            preorder(&method, &mut nodes);
            for node in nodes {
                let Some((start, end)) = node.span else {
                    continue;
                };
                for other in &others {
                    let _ = print::replacement(&source, &method, &node, other);
                }
                let Some(text) = print::replacement(&source, &method, &node, &unspanned(&node))
                else {
                    continue;
                };
                // Where a switch goes: a statement or an expression slot.
                if !node.has(syntax::STATEMENT | syntax::EXPRESSION) {
                    continue;
                }
                printed += 1;
                let mut spliced = source[..start].to_vec();
                spliced.extend_from_slice(text.as_bytes());
                spliced.extend_from_slice(&source[end..]);
                // Only code that parses is converted, as a file is.
                let parses = ruby_prism::parse(&spliced).errors().next().is_none();
                let converted = if parses { defs(&spliced) } else { Vec::new() };
                let reparsed = converted
                    .into_iter()
                    .find(|each| each.span.map(|span| span.0) == method.span.map(|span| span.0));
                let same = reparsed
                    .as_ref()
                    .is_some_and(|reparsed| normalized(reparsed) == normalized(&method));
                if !same {
                    failures.push(format!(
                        "{}: {} -> {text}\n  want: {}\n  got:  {}",
                        path.display(),
                        String::from_utf8_lossy(&source[start..end]),
                        sexp(&normalized(&method)),
                        reparsed
                            .map(|reparsed| sexp(&normalized(&reparsed)))
                            .unwrap_or_default()
                    ));
                }
            }
        }
    }
    assert!(printed > 1000, "{printed}");
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// A file with no mutants gets no switches; a strong pass that wants some
/// mutants gets their switches alone; a mutant of a method's parameters
/// (a destructuring flattened) has no switch, and says why.
#[test]
fn ruby_switches_go_to_the_mutants_wanted_and_name_those_with_none() {
    let none = schemata::switches(b"# nothing\n", 1, None);
    assert!(none.edits.is_empty() && none.unplaceable.is_empty());
    let source = b"def m(a)\n  a + 1\nend\n";
    let all = schemata::switches(source, 1, None);
    let wanted: std::collections::BTreeSet<usize> = [0].into();
    let one = schemata::switches(source, 1, Some(&wanted));
    assert!(!one.edits.is_empty() && one.edits.len() < all.edits.len());
    assert!(one.edits.iter().all(|(_, text)| !text.contains("1000001")));
    let flattened = b"def m((a, b))\n  a\nend\n";
    let switches = schemata::switches(flattened, 1, None);
    assert_eq!(
        switches.unplaceable.values().collect::<Vec<_>>(),
        ["arguments"]
    );
}

/// A mutant's id names mutant's mutator class for the node it changes,
/// including the nodes a mutant rarely lands on.
#[test]
fn ruby_mutants_name_mutant_s_mutator_classes() {
    for (ty, name) in [
        ("pair", "Pair"),
        ("block_pass", "BlockPass"),
        ("procarg0", "ProcargZero"),
        ("in_pattern", "InPattern"),
        ("if_guard", "Guard"),
        ("unless_guard", "Guard"),
        ("match_pattern_p", "MatchPatternP"),
        ("resbody", "Resbody"),
        ("ensure", "Ensure"),
        ("redo", "Generic"),
    ] {
        assert_eq!(mutator_name(ty), name, "{ty}");
    }
}

/// mutant's results where nothing matches: no subjects at all, a file not
/// under this project, a subject without its line, a result without its
/// mutation's code or without its results; each evil mutation is kept as
/// unmatched, and the rest are left out.
#[test]
fn ruby_mutant_sessions_keep_what_they_cannot_match() {
    let read =
        |file: &str| (file == "lib/calc.rb").then(|| b"def big?(n)\n  n > 10\nend\n".to_vec());
    let path = |path: &str| path.trim_start_matches('/').to_owned();
    let empty = serde_json::json!({ "subject_results": [] });
    let empty = serde_json::to_vec(&empty).unwrap();
    assert!(parse_mutant_session(&empty, path, read).unwrap().is_empty());
    let session = serde_json::json!({
        "subject_results": [
            {
                "source_path": "/nowhere/missing.rb",
                "identification": "Calc#gone",
                "coverage_results": [
                    { "mutation_result": { "mutation_type": "evil", "mutation_source": "def gone\nend" }, "criteria_result": {} },
                ],
            },
            {
                "source_path": "/lib/calc.rb",
                "identification": "Calc#big?",
                "coverage_results": [
                    { "mutation_result": { "mutation_type": "evil", "mutation_identification": "evil:x" }, "criteria_result": {} },
                    { "mutation_result": { "mutation_type": "evil", "mutation_source": "def big?(n)\n  n == 10\nend" }, "criteria_result": { "test_result": false } },
                ],
            },
            {
                "source_path": "/lib/calc.rb",
                "identification": "Calc#big?:/lib/calc.rb:7",
                "coverage_results": [
                    { "mutation_result": { "mutation_type": "evil", "mutation_source": "def big?(n)\n  n == 10\nend" }, "criteria_result": {} },
                    { "mutation_result": { "mutation_type": "evil", "mutation_source": "nil" }, "criteria_result": {} },
                ],
            },
            { "source_path": "/lib/calc.rb" },
        ],
    });
    let bytes = serde_json::to_vec(&session).unwrap();
    let joined = parse_mutant_session(&bytes, path, read).unwrap();
    let kinds: Vec<(&str, &str)> = joined
        .iter()
        .map(|mutant| (mutant.mutator.as_str(), mutant.status.as_str()))
        .collect();
    assert_eq!(
        kinds,
        [
            ("Unmatched", "Survived"),
            ("Unmatched", "Survived"),
            ("Send", "Survived"),
            // Another line than the method's, and code with no method.
            ("Unmatched", "Survived"),
            ("Unmatched", "Survived"),
        ]
    );
}

/// Every fixture's mutants are linked to value probes where a probe
/// computes them, and to none elsewhere, without failing on any.
#[test]
fn ruby_mutants_of_every_fixture_link_to_value_probes_or_none() {
    let mut linked = 0;
    for path in fixtures() {
        let source = fs::read(&path).unwrap();
        let mut next = 0;
        let obligations =
            crate::ruby_instrumenter::build_ruby_obligations_with("a.rb", &source, &mut next, true)
                .unwrap();
        linked += candidates("a.rb", &source, &obligations)
            .into_iter()
            .filter(|mutant| mutant.probe.is_some())
            .count();
    }
    assert!(linked > 100, "{linked}");
}

/// A tree written as `sexp` prints it: `(type child ...)`, `:symbol`, `nil`,
/// an integer, `s"<hex>"` for a string.
fn tree(text: &str) -> Rc<N> {
    fn parse(tokens: &mut std::iter::Peekable<std::vec::IntoIter<String>>) -> Child {
        let token = tokens.next().expect("a token");
        if token == "(" {
            let ty: &'static str = Box::leak(tokens.next().expect("a type").into_boxed_str());
            let mut children = Vec::new();
            while tokens.peek().is_some_and(|next| next != ")") {
                children.push(parse(tokens));
            }
            tokens.next();
            return Child::Node(N::new(ty, children));
        }
        if let Some(name) = token.strip_prefix(':') {
            return Child::Sym(name.to_owned());
        }
        if let Some(hex) = token.strip_prefix("s\"") {
            let hex = hex.trim_end_matches('"');
            let bytes = (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap())
                .collect();
            return Child::Str(bytes);
        }
        if token == "nil" {
            return Child::Nil;
        }
        Child::Int(token.parse().expect("an integer"))
    }
    let spaced = text.replace('(', " ( ").replace(')', " ) ");
    let tokens: Vec<String> = spaced.split_whitespace().map(str::to_owned).collect();
    match parse(&mut tokens.into_iter().peekable()) {
        Child::Node(node) => node,
        other => panic!("{other:?}"),
    }
}

/// Unparser's round trip, as emulated: what it prints as another tree
/// (a regexp as a condition, however wrapped; `(a || b) && c` outside a
/// statement; a left operand binding as loosely as a non-associative
/// operator), a jump or `retry` out of place, a local read before it is
/// assigned, including in a singleton method's receiver or a block's
/// parameters.
#[test]
fn ruby_mutants_unparser_cannot_print_back_are_refused() {
    let regexp = "(regexp (str s\"61\") (regopt))";
    let refused = [
        format!("(def :m (args) (if {regexp} nil nil))"),
        format!("(def :m (args) (while (begin {regexp}) nil))"),
        format!("(def :m (args) (if (send {regexp} :!) nil nil))"),
        format!("(def :m (args) (until (and (true) {regexp}) nil))"),
        "(def :m (args) (send nil :a (and (or (true) (false)) (true))))".into(),
        "(def :m (args) (send (send (int 1) :== (int 2)) :== (int 3)))".into(),
        "(def :m (args) (send (send (int 1) :** (int 2)) :** (int 3)))".into(),
        "(def :m (args) (break))".into(),
        "(def :m (args) (retry))".into(),
        "(def :m (args) (lvar :x))".into(),
        "(def :m (args) (defs (lvar :x) :n (args) nil))".into(),
        "(def :m (args) (block (send nil :a) (args (optarg :x (lvar :y))) nil))".into(),
        "(def :m (args) (block (lvar :y) (args) nil))".into(),
    ];
    for text in &refused {
        assert!(!validates(&tree(text)), "{text}");
    }
    let kept = [
        format!("(def :m (args) (if (send {regexp} :=~ (int 1)) nil nil))"),
        format!("(def :m (args) (if (begin {regexp} {regexp}) nil nil))"),
        format!("(def :m (args) (if (send {regexp} :! (int 1)) nil nil))"),
        "(def :m (args) (and (or (true) (false)) (true)))".into(),
        "(def :m (args) (send (send (int 1) :- (int 2)) :- (int 3)))".into(),
        "(def :m (args) (while (true) (break)))".into(),
        "(def :m (args) (defs (self) :n (args) nil))".into(),
        "(def :m (args) (numblock (send nil :a) 0 nil))".into(),
        "(def :m (args) (numblock (send nil :a) 2 (lvar :_2)))".into(),
        "(def :m (args) (block (send nil :a) (args (arg :x)) (lvar :x)))".into(),
    ];
    for text in &kept {
        assert!(validates(&tree(text)), "{text}");
    }
}

/// A statement list that lost one statement locates the statement removed;
/// one that lost one and changed another, or lost a slot that holds no
/// node, is located at the list itself, as `scripts/mutant-parity.rb` does.
#[test]
fn ruby_mutants_locate_a_removed_statement_only_where_it_is_one() {
    let method = tree("(def :m (args) (begin (int 1) (int 2) (int 3)))");
    let cases = [
        ("(def :m (args) (begin (int 1) (int 3)))", "(int 2)", None),
        (
            "(def :m (args) (begin (int 1) (int 4)))",
            "(begin (int 1) (int 2) (int 3))",
            Some("(begin (int 1) (int 4))"),
        ),
    ];
    for (mutated, located, replacement) in cases {
        let mutated = tree(mutated);
        let (found, by) = localize(&method, &mutated);
        assert_eq!(sexp(&found), located);
        assert_eq!(by.map(|by| sexp(&by)), replacement.map(str::to_owned));
        let last = path(&method, &mutated).pop().unwrap();
        assert_eq!(sexp(&last.0), located);
    }
    let holding_nil = tree("(def :m (args) (begin (int 1) nil (int 3)))");
    let mutated = tree("(def :m (args) (begin (int 1) (int 3)))");
    assert_eq!(
        sexp(&localize(&holding_nil, &mutated).0),
        "(begin (int 1) nil (int 3))"
    );
    assert_eq!(
        sexp(&path(&holding_nil, &mutated).pop().unwrap().0),
        "(begin (int 1) nil (int 3))"
    );
}

/// A block whose call is written without parentheses (`a.b 1 do`) is
/// printed with them when the block is printed from its parts; source text
/// that is not UTF-8 is printed from its node's parts instead.
#[test]
fn ruby_blocks_and_odd_source_print_from_their_parts() {
    let source = b"def m(list)\n  list.each_slice 2 do |a|\n    a\n  end\nend\n";
    let method = defs(source).remove(0);
    let block = method.node(2).unwrap();
    let mut changed = (**block).clone();
    changed.span = None;
    assert_eq!(
        print::one_line(source, &method, &changed).as_deref(),
        Some("list.each_slice(2) { |a| a }")
    );
    let odd = b"\xff";
    let literal = Rc::new(N {
        ty: "int",
        children: vec![Child::Int(1)],
        span: Some((0, 1)),
        syntax: 0,
    });
    assert_eq!(
        print::one_line(odd, &literal, &literal).as_deref(),
        Some("1")
    );
}
