//! Binding Go's test functions to the evidence they produce.
//!
//! Attribution needs two things the source does not have: every test function
//! announcing itself, and somewhere to write the evidence once the package has
//! finished. Go gives exactly one after-all hook, `TestMain`, so this augments
//! the package's own when it has one and synthesises it when it does not.
//!
//! The wrapping is deliberate rather than a deferred call. The idiomatic
//! `TestMain` ends in `os.Exit(m.Run())`, and `os.Exit` runs no deferred
//! function, so a `defer` there would silently produce no evidence for exactly
//! the projects that wrote the most careful harness.

use tree_sitter::Node;

use crate::go_instrumenter::{GoEdit, GoInstrumenterError, RUNTIME_IMPORT, import_edit, parse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoTestFile {
    /// Test functions this file declares, in source order.
    pub tests: Vec<String>,
    /// Tests that call `t.Parallel()`. Go runs these alongside each other, so
    /// work they do after that call belongs to no single test and attributing
    /// it to whichever was current would be a guess presented as a
    /// measurement.
    pub parallel: Vec<String>,
    /// True when this file declares `TestMain`, which the package may only
    /// have one of.
    pub declares_test_main: bool,
    pub edits: Vec<GoEdit>,
}

fn function_name(node: Node, source: &str) -> Option<String> {
    node.child_by_field_name("name")
        .map(|name| source[name.byte_range()].to_owned())
}

/// Go's own rule for what the toolchain will run as a test, from
/// `cmd/go/internal/load/test.go`: the prefix, then either nothing or a rune
/// that is not lowercase.
///
/// So a bare `Test` is a test, and `Testify` is not — the `i` makes it an
/// ordinary function that merely starts with the word. Guessing differently
/// from the toolchain would attribute evidence to something `go test` never
/// runs, or miss a test that it does.
fn is_test_function(name: &str) -> bool {
    name.strip_prefix("Test")
        .is_some_and(|rest| rest.chars().next().is_none_or(|c| !c.is_lowercase()))
}

fn first_parameter<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let parameters = node.child_by_field_name("parameters")?;
    let mut cursor = parameters.walk();
    parameters
        .children(&mut cursor)
        .find(|child| child.kind() == "parameter_declaration")
}

fn parameter_type(node: Node, source: &str) -> Option<String> {
    let kind = first_parameter(node)?.child_by_field_name("type")?;
    Some(source[kind.byte_range()].trim().to_owned())
}

/// What the test calls its `*testing.T`, which the announcement needs in order
/// to read the outcome from it. Almost always `t`, but nothing requires that,
/// and a harness that assumed it would fail to compile on the files that
/// chose otherwise.
fn parameter_name(node: Node, source: &str) -> Option<String> {
    let name = first_parameter(node)?.child_by_field_name("name")?;
    let text = source[name.byte_range()].trim();
    // `func TestX(*testing.T)` is legal and names nothing there is to read.
    (text != "_" && !text.is_empty()).then(|| text.to_owned())
}

/// Instrument one `_test.go` file so every test announces itself.
pub fn instrument_test_file(
    source: &str,
    alias: &str,
    evidence_path: &str,
) -> Result<GoTestFile, GoInstrumenterError> {
    let tree = parse(source)?;
    let mut file = GoTestFile {
        tests: Vec::new(),
        parallel: Vec::new(),
        declares_test_main: false,
        edits: Vec::new(),
    };
    let root = tree.root_node();
    // Only the edits that name the runtime directly need its import. The
    // per-test announcement goes through a package-local helper instead, so a
    // file of ordinary tests must not gain an import it never mentions.
    let mut needs_runtime = false;
    let mut cursor = root.walk();
    for child in root.children(&mut cursor) {
        if child.kind() != "function_declaration" {
            continue;
        }
        let Some(name) = function_name(child, source) else {
            continue;
        };
        let Some(body) = child.child_by_field_name("body") else {
            continue;
        };
        if name == "TestMain" {
            file.declares_test_main = true;
            needs_runtime = true;
            file.edits.push(GoEdit {
                at: body.start_byte() + 1,
                rank: 100,
                text: format!("\n\t{alias}.Arm(__supercovProbeCount, __supercovDecisionWidths)\n"),
            });
            // Wrap the m.Run() call itself rather than deferring after it.
            wrap_run_calls(body, source, alias, evidence_path, &mut file.edits);
            continue;
        }
        if !is_test_function(&name) {
            continue;
        }
        // `func TestX(t *testing.T)` is a test; `func TestX(b *testing.B)` is
        // not, whatever its name suggests.
        if parameter_type(child, source).as_deref() != Some("*testing.T") {
            continue;
        }
        file.tests.push(name.clone());
        if calls_parallel(body, source) {
            // Announcing it would bind whatever runs next to this test, and
            // what runs next includes the other parallel tests. Its coverage
            // still counts run-wide; it simply belongs to no test, which is
            // the truth rather than a guess dressed as a measurement.
            file.parallel.push(name.clone());
            continue;
        }
        // Through the generated helper rather than the runtime directly, so
        // the announcement can read the outcome off the test's own *testing.T
        // without the runtime ever importing `testing`.
        // The trailing newline matters: a body written on one line puts its
        // first statement immediately after the brace, and an announcement
        // with nothing after it would run into that statement and not compile.
        let announcement = match parameter_name(child, source) {
            Some(parameter) => {
                format!("\n\tdefer {HARNESS_ENTER}({parameter}, \"{name}\")()\n")
            }
            None => format!("\n\tdefer {alias}.EnterTest(\"{name}\")()\n"),
        };
        if announcement.contains(&format!("{alias}.")) {
            needs_runtime = true;
        }
        file.edits.push(GoEdit {
            at: body.start_byte() + 1,
            rank: 100,
            text: announcement,
        });
    }
    // A file that names the runtime needs the import that makes it resolve;
    // one that only calls the generated helper must not get an unused import.
    if needs_runtime && let Some(import) = import_edit(source, alias, RUNTIME_IMPORT) {
        file.edits.push(import);
    }
    Ok(file)
}

/// Whether a test hands itself to Go's parallel scheduler.
///
/// Detected from the source rather than at runtime, because by the time
/// `t.Parallel()` returns the test has already been descheduled and anything
/// observed afterwards may belong to another one.
fn calls_parallel(node: Node, source: &str) -> bool {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && source[function.byte_range()]
            .trim_end()
            .ends_with(".Parallel")
    {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .filter(Node::is_named)
        .any(|child| calls_parallel(child, source))
}

fn wrap_run_calls(node: Node, source: &str, alias: &str, evidence: &str, edits: &mut Vec<GoEdit>) {
    if node.kind() == "call_expression"
        && let Some(function) = node.child_by_field_name("function")
        && source[function.byte_range()].trim_end().ends_with(".Run")
    {
        edits.push(GoEdit {
            at: node.start_byte(),
            rank: 50,
            text: format!("{alias}.Finish("),
        });
        edits.push(GoEdit {
            at: node.end_byte(),
            rank: 50,
            text: format!(", \"{evidence}\")"),
        });
        return;
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            wrap_run_calls(child, source, alias, evidence, edits);
        }
    }
}

/// The generated file that gives a package the array its probes store into.
///
/// A normal source file, not a test one: instrumented statements live in
/// ordinary code and must compile in every build of the package, not only
/// under `go test`.
pub fn probe_array_file(package: &str, alias: &str, import: &str, probe_count: usize) -> String {
    format!(
        "// Code generated by Supercov. DO NOT EDIT.\n\npackage {package}\n\nimport {alias} \"{import}\"\n\n// Reserved once per package and shared across the module, so a probe is an\n// index into an array this file already holds rather than a call that has to\n// find one.\nvar {} = {alias}.Reserve({probe_count})\n",
        crate::go_instrumenter::HITS_VARIABLE
    )
}

/// The package-local function every instrumented test defers to.
pub const HARNESS_ENTER: &str = "__supercovTest";

/// The generated `TestMain` for a package that has none, plus the probe count
/// every instrumented file in the package refers to.
pub fn synthesized_harness(
    package: &str,
    alias: &str,
    import: &str,
    probe_count: usize,
    decision_widths: &[u8],
    evidence_path: &str,
    declares_test_main: bool,
) -> String {
    let mut out =
        format!("// Code generated by Supercov. DO NOT EDIT.\n\npackage {package}\n\nimport (\n");
    // Always, because the announcement helper below takes a *testing.T. This
    // file is a `_test.go`, so importing `testing` here reaches only the test
    // binary and never a build of the product.
    out.push_str("\t\"testing\"\n\n");
    out.push_str(&format!("\t{alias} \"{import}\"\n)\n\n"));
    // The runtime sizes each decision's vector from this, so a width the
    // harness got wrong would record vectors of the wrong shape.
    let widths = decision_widths
        .iter()
        .map(u8::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    out.push_str(&format!(
        "const __supercovProbeCount = {probe_count}\n\nvar __supercovDecisionWidths = []uint8{{{widths}}}\n\n"
    ));
    // Binding and outcome in one place, so the edit inside the author's test
    // is a single deferred call. Reading *testing.T here rather than in the
    // runtime is what keeps `testing` out of every product binary that
    // imports the runtime.
    out.push_str(&format!(
        "func {HARNESS_ENTER}(t *testing.T, name string) func() {{\n\tdone := {alias}.EnterTest(name)\n\treturn func() {{\n\t\tif t.Skipped() {{\n\t\t\t{alias}.Outcome(\"skipped\")\n\t\t}} else if t.Failed() {{\n\t\t\t{alias}.Outcome(\"failed\")\n\t\t}}\n\t\tdone()\n\t}}\n}}\n\n"
    ));
    if declares_test_main {
        // The author's TestMain arms the runtime; this keeps the widths
        // referenced even in a package where nothing else names them.
        out.push_str("var _ = __supercovDecisionWidths\n");
        return out;
    }
    out.push_str(&format!(
        "func TestMain(m *testing.M) {{\n\t{alias}.Arm(__supercovProbeCount, __supercovDecisionWidths)\n\tos.Exit({alias}.Finish(m.Run(), \"{evidence_path}\"))\n}}\n"
    ));
    out.replace("\t\"testing\"\n", "\t\"os\"\n\t\"testing\"\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::go_instrumenter::rewrite;

    fn instrumented(source: &str) -> (GoTestFile, String) {
        let file = instrument_test_file(source, "__supercov", "evidence.bin").expect("instrument");
        let out = rewrite(source, &file.edits);
        parse(&out).unwrap_or_else(|error| panic!("{error}\n{out}"));
        (file, out)
    }

    #[test]
    fn go_s_own_rule_decides_what_counts_as_a_test() {
        // `Test` followed by a non-lowercase rune, taking *testing.T. A
        // benchmark named like a test is not one, and neither is a helper.
        let (file, _) = instrumented(
            "package p\n\nimport \"testing\"\n\nfunc Test(t *testing.T) {}\nfunc TestOne(t *testing.T) {}\nfunc Testify(t *testing.T) {}\nfunc TestBench(b *testing.B) {}\nfunc helper(t *testing.T) {}\n",
        );
        // Bare `Test` counts; `Testify` does not, because the rune after the
        // prefix is lowercase. This is the toolchain's rule, not a guess at it.
        assert_eq!(file.tests, ["Test", "TestOne"]);
    }

    #[test]
    fn an_existing_test_main_is_wrapped_rather_than_deferred_into() {
        // `os.Exit(m.Run())` runs no deferred function, so a defer here would
        // produce no evidence for exactly the projects with the most careful
        // harness. The run call is wrapped instead.
        let (file, out) = instrumented(
            "package p\n\nimport (\n\t\"os\"\n\t\"testing\"\n)\n\nfunc TestMain(m *testing.M) {\n\tos.Exit(m.Run())\n}\n",
        );
        assert!(file.declares_test_main);
        assert!(
            out.contains("__supercov.Finish(m.Run(), \"evidence.bin\")"),
            "{out}"
        );
        assert!(
            out.contains("__supercov.Arm(__supercovProbeCount, __supercovDecisionWidths)"),
            "{out}"
        );
        assert!(
            !out.contains("defer __supercov.Write"),
            "a defer would never run:\n{out}"
        );
    }

    #[test]
    fn a_package_without_test_main_gets_one_and_never_two() {
        // Go allows a package exactly one TestMain, so the generated file must
        // declare it only when the package has none.
        let generated = synthesized_harness(
            "p",
            "__supercov",
            "example.com/rt",
            42,
            &[2, 3],
            "e.bin",
            false,
        );
        assert!(
            generated.contains("[]uint8{2, 3}"),
            "the runtime sizes its vectors from this:\n{generated}"
        );
        assert!(
            generated.contains("func TestMain(m *testing.M)"),
            "{generated}"
        );
        assert!(generated.contains("const __supercovProbeCount = 42"));
        assert!(
            generated.contains("os.Exit"),
            "the generated harness preserves the exit code"
        );

        let alongside =
            synthesized_harness("p", "__supercov", "example.com/rt", 42, &[], "e.bin", true);
        assert!(!alongside.contains("func TestMain"), "{alongside}");
        assert!(
            alongside.contains("func __supercovTest(t *testing.T"),
            "every package gets the announcement helper, TestMain or not:\n{alongside}"
        );
    }

    #[test]
    fn a_parallel_test_is_named_rather_than_attributed_by_guesswork() {
        // Go runs these alongside each other. Binding probes to whichever was
        // most recently announced would produce per-test numbers that look
        // exact and are not; the coverage still counts run-wide.
        let (file, out) = instrumented(
            "package p\n\nimport \"testing\"\n\nfunc TestSerial(t *testing.T) {\n\tdoWork()\n}\n\nfunc TestParallel(t *testing.T) {\n\tt.Parallel()\n\tdoWork()\n}\n",
        );
        assert_eq!(file.tests, ["TestSerial", "TestParallel"]);
        assert_eq!(file.parallel, ["TestParallel"]);
        assert!(out.contains("__supercovTest(t, \"TestSerial\")"), "{out}");
        assert!(
            !out.contains("\"TestParallel\""),
            "a parallel test must not claim what ran beside it:\n{out}"
        );
    }

    #[test]
    fn the_announcement_uses_whatever_the_test_called_its_t() {
        // `t` is the convention, not a rule. A harness that assumed it would
        // fail to compile on the files that chose otherwise, and a file of
        // ordinary tests must not gain a runtime import it never names.
        let (_, out) = instrumented(
            "package p\n\nimport \"testing\"\n\nfunc TestOne(tt *testing.T) {\n\tdoWork()\n}\n",
        );
        assert!(out.contains("__supercovTest(tt, \"TestOne\")"), "{out}");
        assert!(
            !out.contains(RUNTIME_IMPORT),
            "an ordinary test file names only the generated helper:\n{out}"
        );
    }

    #[test]
    fn a_test_that_names_no_t_still_gets_bound() {
        // `func TestX(*testing.T)` is legal Go and there is nothing to read an
        // outcome from, so it announces itself directly and is reported as
        // having passed unless the run says otherwise.
        let (file, out) = instrumented(
            "package p\n\nimport \"testing\"\n\nfunc TestOne(*testing.T) {\n\tdoWork()\n}\n",
        );
        assert_eq!(file.tests, ["TestOne"]);
        assert!(out.contains("__supercov.EnterTest(\"TestOne\")"), "{out}");
        assert!(
            out.contains(RUNTIME_IMPORT),
            "naming the runtime directly requires its import:\n{out}"
        );
    }

    #[test]
    fn a_test_announces_itself_before_anything_it_calls() {
        let (_, out) = instrumented(
            "package p\n\nimport \"testing\"\n\nfunc TestOne(t *testing.T) {\n\tdoWork()\n}\n",
        );
        let body = out.find("TestOne").unwrap();
        let enter = out
            .find("__supercovTest(t, \"TestOne\")")
            .expect("announcement");
        let work = out.find("doWork()").unwrap();
        assert!(body < enter && enter < work, "{out}");
    }

    #[test]
    fn a_test_written_on_one_line_still_compiles() {
        // A body on one line puts its first statement immediately after the
        // brace. An announcement inserted there with nothing after it runs
        // straight into that statement, and the file stops being Go -- so
        // Supercov breaks the suite it was asked to measure.
        let (file, out) = instrumented(
            "package p\n\nimport \"testing\"\n\nfunc TestOne(t *testing.T) { if work() != 1 { t.Fatal(\"no\") } }\n",
        );
        assert_eq!(file.tests, ["TestOne"]);
        // `instrumented` parses the result, so reaching here is most of the
        // claim; this says the announcement is on a line of its own.
        assert!(out.contains("__supercovTest(t, \"TestOne\")()\n"), "{out}");

        // The same for a TestMain nobody spread over several lines.
        let (_, out) = instrumented(
            "package p\n\nimport \"testing\"\n\nfunc TestMain(m *testing.M) { os.Exit(m.Run()) }\n",
        );
        assert!(out.contains("__supercovDecisionWidths)\n"), "{out}");
    }
}
