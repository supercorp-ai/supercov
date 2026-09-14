//! How much of a map one edit invalidates, measured rather than argued about.
//!
//! B19 reports 410 of supergateway's 618 flows invalidated by edits that do not
//! touch the statements they anchor. That map is an accumulated review backlog,
//! so it cannot separate the causes. This builds a map of a known shape and
//! makes one edit at a time, so each cause can be priced on its own.

use std::collections::BTreeSet;
use supercov_engine::assertion_map::*;

mod common;

const FILES: usize = 10;
const FLOWS_PER_FILE: usize = 20;

fn anchor_at(file: &str, text: &str, needle: &str) -> Anchor {
    let start = text.find(needle).unwrap();
    Anchor::new(file, text, start, start + needle.len())
}

/// A file of many independent functions, the shape real source has.
fn source(file: usize) -> String {
    let mut out = String::new();
    for n in 0..FLOWS_PER_FILE {
        out.push_str(&format!(
            "// what step {n} is for\nexport function step{file}_{n}(value) {{\n  return value + {n};\n}}\n\n"
        ));
    }
    out
}

fn build() -> (Inputs, AssertionMap, State) {
    let test = "import assert from 'node:assert/strict';\nassert.equal(result, 1);\n";
    let mut files = Files::from([("test.js".to_owned(), test.to_owned())]);
    for file in 0..FILES {
        files.insert(format!("src/m{file}.js"), source(file));
    }
    let inputs = Inputs {
        schema_version: 1,
        language: "javascript".into(),
        context_digest: "context".into(),
        files,
        assertions: vec![InventorySite {
            at: anchor_at("test.js", test, "assert.equal(result, 1)"),
            operation: "assert.equal".into(),
        }],
        limitations: vec![],
    };
    let (mut map, state) = seed(&inputs, "archive");
    let a = &mut map.assertions[0];
    a.observes = vec!["result equals one".into()];
    // One flow per function, each anchored only at its own function's body, and
    // watching nothing: the watch list is the author's explicit "tell me", and
    // this measures what happens without one.
    a.flows = (0..FILES)
        .flat_map(|file| (0..FLOWS_PER_FILE).map(move |n| (file, n)))
        .map(|(file, n)| {
            let path = format!("src/m{file}.js");
            let body = inputs.files[&path].clone();
            Flow {
                id: format!("f{file}_{n}"),
                basis: None,
                questions: vec![],
                explanation: format!("step {file}_{n} returns its input plus {n}"),
                applies_to: vec![TestSelector {
                    file: "test.js".into(),
                    name: "test".into(),
                }],
                nodes: vec![Node {
                    id: "return".into(),
                    at: anchor_at(&path, &body, &format!("return value + {n};")),
                    role: "value".into(),
                    meaning: String::new(),
                }],
                edges: vec![Edge {
                    from: "return".into(),
                    to: "$assertion".into(),
                    kind: "data".into(),
                    basis: String::new(),
                }],
                counts_as_asserted: vec!["return".into()],
                watch: vec![],
            }
        })
        .collect();
    (inputs, map, state)
}

/// Mark every flow reviewed, the way an author who has just read them would.
fn acknowledge(map: &mut AssertionMap, state: &State, inputs: &Inputs) {
    let snapshot = map.clone();
    let manifest = inputs.manifest();
    for a in &mut map.assertions {
        for f in &mut a.flows {
            let prior = snapshot
                .assertions
                .iter()
                .find(|other| other.id == a.id)
                .unwrap();
            f.basis = Some(expected_basis(prior, f, &snapshot, state, &manifest));
        }
    }
}

fn stale(map: &AssertionMap, state: &State, inputs: &Inputs) -> usize {
    map.assertions
        .iter()
        .flat_map(|a| a.flows.iter().map(move |f| (a, f)))
        .filter(|(a, f)| !reasons(a, f, map, state, inputs).is_empty())
        .count()
}

fn after(edit: impl FnOnce(&mut String)) -> usize {
    let (inputs, mut map, state) = build();
    acknowledge(&mut map, &state, &inputs);
    assert_eq!(stale(&map, &state, &inputs), 0, "everything reviewed first");
    let mut next = inputs.clone();
    edit(next.files.get_mut("src/m0.js").unwrap());
    let (carried, carried_state) =
        carry(&map, &state, &inputs.manifest(), &next, "new", false).unwrap();
    stale(&carried, &carried_state, &next)
}

#[test]
#[ignore = "a measurement, not a pass/fail; run with --ignored"]
fn one_edit_is_priced_against_every_cause() {
    let total = FILES * FLOWS_PER_FILE;

    // A comment, changing nothing a program can observe.
    let comment = after(|s| *s = s.replace("// what step 3 is for", "// rewritten note"));

    // A statement in one function, which its own flow depends on.
    let same = after(|s| *s = s.replace("return value + 3;", "return value + 3 + 0;"));

    // A statement in a different function of the same file: no flow anchored
    // here has a node in it.
    let other = after(|s| s.push_str("\nexport function unrelated() {\n  return 0;\n}\n"));

    println!("\n  map: {total} flows over {FILES} files, {FLOWS_PER_FILE} per file\n");
    println!("  edit                                      stale   of file's 20");
    println!(
        "  comment text only                         {comment:>5}   {:>5}",
        comment.min(FLOWS_PER_FILE)
    );
    println!(
        "  one statement, in a flow's own function   {same:>5}   {:>5}",
        same.min(FLOWS_PER_FILE)
    );
    println!(
        "  one function appended, unrelated          {other:>5}   {:>5}",
        other.min(FLOWS_PER_FILE)
    );
    println!();

    println!("  the honest floor for those three is 0, 1 and 0.\n");
    // Whatever else changes, the flow whose own statement changed must go.
    assert!(
        same >= 1,
        "the flow whose statement changed must be rechecked"
    );
    let _ = BTreeSet::<String>::new();
}

/// The floor, held as a test rather than a measurement: a comment cannot change
/// what a program does, so it cannot invalidate a claim about what it does.
/// This fails today -- all twenty go -- and is what the semantic digest is for.
#[test]
#[ignore = "pins the fix; fails until a file is compared by what it means"]
fn a_comment_invalidates_nothing() {
    let stale = after(|s| *s = s.replace("// what step 3 is for", "// rewritten note"));
    assert_eq!(
        stale, 0,
        "a comment changed and {stale} claims needed rechecking"
    );
}
