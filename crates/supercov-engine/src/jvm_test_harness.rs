//! Binding JVM test methods to the evidence they produce.
//!
//! Every framework worth supporting here marks its tests with an annotation —
//! JUnit 4 and 5, TestNG, and the annotation-based half of Kotlin testing all
//! do. Recognising the annotation rather than the framework means one rule
//! covers them, and a framework that adopts the same convention tomorrow works
//! without being named.
//!
//! The announcement is wrapped in `try`/`finally`, not appended. A failing
//! test throws, and a test that ends by throwing is exactly the one whose
//! coverage a reader most wants attributed: without the `finally` its evidence
//! would leak into whichever test ran next.

use tree_sitter::Node;

use crate::go_instrumenter::GoEdit;
use crate::jvm_instrumenter::{JvmInstrumenterError, JvmLanguage, RUNTIME_CLASS, parse};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JvmTestFile {
    /// Test methods this file declares, qualified by their enclosing type.
    pub tests: Vec<String>,
    pub edits: Vec<GoEdit>,
}

/// Annotations that mark a method as a test in the frameworks this supports.
///
/// Matched on the simple name, because a file may import the annotation or
/// write it fully qualified and both mean the same thing.
const TEST_ANNOTATIONS: &[&str] = &[
    // JUnit 5
    "Test",
    "ParameterizedTest",
    "RepeatedTest",
    "TestFactory",
    "TestTemplate",
    // JUnit 4 and TestNG both spell theirs `@Test`, already covered above.
];

fn annotation_names(node: Node, source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        match child.kind() {
            "modifiers" | "modifier_list" | "annotation" | "marker_annotation" => {
                let text = source[child.byte_range()].to_owned();
                for part in text.split('@').skip(1) {
                    let name = part
                        .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '.'))
                        .next()
                        .unwrap_or("");
                    if let Some(simple) = name.rsplit('.').next()
                        && !simple.is_empty()
                    {
                        names.push(simple.to_owned());
                    }
                }
            }
            _ => {}
        }
    }
    names
}

fn is_test_method(node: Node, source: &str) -> bool {
    annotation_names(node, source)
        .iter()
        .any(|name| TEST_ANNOTATIONS.contains(&name.as_str()))
}

fn enclosing_type(node: Node, source: &str) -> Option<String> {
    let mut current = node.parent();
    while let Some(parent) = current {
        if matches!(
            parent.kind(),
            "class_declaration" | "object_declaration" | "interface_declaration"
        ) && let Some(name) = parent.child_by_field_name("name")
        {
            return Some(source[name.byte_range()].to_owned());
        }
        current = parent.parent();
    }
    None
}

/// The block a test's statements live in.
///
/// Java hangs it off the `body` field. Kotlin puts a `function_body` in
/// between and does not name it as a field, so the block has to be found by
/// walking rather than asked for.
fn body_of<'t>(node: Node<'t>, _language: JvmLanguage) -> Option<Node<'t>> {
    if let Some(body) = node.child_by_field_name("body")
        && body.kind() == "block"
    {
        return Some(body);
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "block" {
            return Some(child);
        }
        if child.kind() == "function_body" {
            let mut inner = child.walk();
            if let Some(block) = child.children(&mut inner).find(|c| c.kind() == "block") {
                return Some(block);
            }
        }
    }
    None
}

/// Instrument a test source file so every test announces itself.
pub fn instrument_test_file(
    source: &str,
    language: JvmLanguage,
) -> Result<JvmTestFile, JvmInstrumenterError> {
    let tree = parse(source, language)?;
    let mut file = JvmTestFile {
        tests: Vec::new(),
        edits: Vec::new(),
    };
    collect(tree.root_node(), source, language, &mut file);
    Ok(file)
}

fn collect(node: Node, source: &str, language: JvmLanguage, file: &mut JvmTestFile) {
    if matches!(node.kind(), "method_declaration" | "function_declaration")
        && is_test_method(node, source)
        && let Some(name) = node.child_by_field_name("name")
        && let Some(body) = body_of(node, language)
    {
        let simple = source[name.byte_range()].to_owned();
        let qualified = match enclosing_type(node, source) {
            Some(owner) => format!("{owner}#{simple}"),
            None => simple,
        };
        file.tests.push(qualified.clone());
        // `try` opens just inside the body and `finally` closes just before it
        // ends, so a test that throws still hands back its evidence.
        // An empty body puts both of these on the same offset, so their ranks
        // are what keeps `try {` to the left of `} finally`. Applying right to
        // left, the lower rank is inserted last and ends up leftmost.
        file.edits.push(GoEdit {
            at: body.start_byte() + 1,
            rank: 80,
            text: format!("\n{RUNTIME_CLASS}.enterTest(\"{qualified}\");\ntry {{"),
        });
        file.edits.push(GoEdit {
            at: body.end_byte() - 1,
            rank: 90,
            text: format!("\n}} finally {{ {RUNTIME_CLASS}.exitTest(); }}\n"),
        });
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.is_named() {
            collect(child, source, language, file);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jvm_instrumenter::rewrite;

    fn instrumented(source: &str, language: JvmLanguage) -> (JvmTestFile, String) {
        let file = instrument_test_file(source, language).expect("instrument");
        let out = rewrite(source, &file.edits);
        parse(&out, language).unwrap_or_else(|error| panic!("{error}\n{out}"));
        (file, out)
    }

    #[test]
    fn an_annotation_marks_a_test_whatever_framework_wrote_it() {
        // JUnit 4, JUnit 5 and TestNG all spell it `@Test`, and a file may
        // import the annotation or write it fully qualified. Recognising the
        // simple name covers every combination without naming a framework.
        let (file, _) = instrumented(
            r#"import org.junit.jupiter.api.Test;
class ThingTest {
    @Test void one() {}
    @org.junit.Test public void two() {}
    @ParameterizedTest void three() {}
    void helper() {}
    @Override public String toString() { return ""; }
}
"#,
            JvmLanguage::Java,
        );
        assert_eq!(
            file.tests,
            ["ThingTest#one", "ThingTest#two", "ThingTest#three"]
        );
    }

    #[test]
    fn an_empty_test_body_still_produces_valid_source() {
        // Both edits land on the same offset when a body is `{}`, so only
        // their relative order decides whether the result compiles.
        let (file, out) = instrumented("class T { @Test void empty() {} }", JvmLanguage::Java);
        assert_eq!(file.tests, ["T#empty"]);
        let opened = out.find("try {").expect("try");
        let closed = out.find("} finally").expect("finally");
        assert!(opened < closed, "{out}");
    }

    #[test]
    fn a_test_that_throws_still_hands_back_its_evidence() {
        // The failing test is exactly the one whose coverage a reader wants
        // attributed. Without the finally its evidence would leak into
        // whichever test ran next and be reported as that test's.
        let (_, out) = instrumented(
            "class T { @Test void boom() { throw new RuntimeException(\"x\"); } }",
            JvmLanguage::Java,
        );
        let enter = out.find("enterTest").expect("announcement");
        let opened = out.find("try {").expect("try");
        let throws = out.find("throw new").expect("body");
        let finally = out.find("finally").expect("finally");
        assert!(enter < opened, "announce before entering the block:\n{out}");
        assert!(opened < throws, "the body runs inside the try:\n{out}");
        assert!(throws < finally, "the finally closes after it:\n{out}");
        assert!(out.contains("exitTest()"), "{out}");
    }

    #[test]
    fn kotlin_tests_are_bound_the_same_way() {
        let (file, out) = instrumented(
            "import kotlin.test.Test\n\nclass ThingTest {\n    @Test fun one() {\n        check(true)\n    }\n}\n",
            JvmLanguage::Kotlin,
        );
        assert_eq!(file.tests, ["ThingTest#one"]);
        assert!(out.contains("enterTest(\"ThingTest#one\")"), "{out}");
        assert!(out.contains("finally"), "{out}");
    }

    #[test]
    fn a_file_with_no_tests_is_left_untouched() {
        // A helper class must not gain a harness it has no use for.
        let (file, out) = instrumented(
            "class Helper { String name() { return \"x\"; } }",
            JvmLanguage::Java,
        );
        assert!(file.tests.is_empty());
        assert!(file.edits.is_empty());
        assert_eq!(out, "class Helper { String name() { return \"x\"; } }");
    }
}
