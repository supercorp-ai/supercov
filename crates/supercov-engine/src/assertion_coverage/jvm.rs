//! Java and Kotlin: statements by tree-sitter, tests by class and method name.
use super::{Change, Located, Starts, line_of, line_starts};
use crate::jvm_instrumenter::JvmLanguage;
use tree_sitter::{Node, Parser, Tree};

fn kotlin(file: &str) -> bool {
    file.ends_with(".kt") || file.ends_with(".kts")
}

/// Every statement starting on each line, with its change: `if` inverted,
/// `return x` returning the default value, a declaration or assignment with a
/// value taking the default value, anything else skipped. Local declarations
/// of classes and functions, and variables without a value, are not assessed.
pub(super) fn statement_starts(file: &str, source: &str) -> Starts {
    let mut out = Starts::new();
    // The instrumenter's parse first: it reads the Kotlin the grammar alone
    // gets wrong, exactly as the measured run did. Failing that, whatever the
    // grammar recovered still holds statements worth assessing.
    let language = if kotlin(file) {
        JvmLanguage::Kotlin
    } else {
        JvmLanguage::Java
    };
    let tree = match crate::jvm_instrumenter::parse(source, language) {
        Ok(tree) => tree,
        Err(_) => recovered(source, language),
    };
    let starts = line_starts(source);
    walk(tree.root_node(), source, &starts, kotlin(file), &mut out);
    out
}

/// A `catch`, `when` entry or `finally` block lies in the expression it is
/// part of.
const OWNED: &str = "a catch, when entry or finally block has its expression";

/// Whatever tree the grammar recovers from a source that does not parse.
fn recovered(source: &str, language: JvmLanguage) -> Tree {
    let mut parser = Parser::new();
    let grammar = match language {
        JvmLanguage::Kotlin => crate::kotlin_grammar::LANGUAGE.into(),
        JvmLanguage::Java => tree_sitter_java::LANGUAGE.into(),
    };
    parser
        .set_language(&grammar)
        .expect("the JVM grammars are built for this tree-sitter");
    // With no timeout or cancellation set, a parse always gives a tree.
    parser
        .parse(source, None)
        .expect("tree-sitter recovers a tree from any text")
}

/// What an `if` tests, out of Java's parentheses.
fn tested(node: Node) -> Option<Node> {
    if !matches!(node.kind(), "if_statement" | "if_expression") {
        return None;
    }
    let condition = node.child_by_field_name("condition")?;
    Some(if condition.kind() == "parenthesized_expression" {
        condition.named_child(0).unwrap_or(condition)
    } else {
        condition
    })
}

fn walk(node: Node, source: &str, starts: &[usize], kotlin: bool, out: &mut Starts) {
    let parent = node.parent().map(|p| p.kind()).unwrap_or("");
    let statement = node.is_named()
        && (matches!(
            parent,
            "block" | "statements" | "switch_block_statement_group" | "constructor_body"
        ) || (matches!(node.kind(), "if_statement" | "if_expression")
            && matches!(parent, "if_statement" | "if_expression")));
    if statement && let Some(change) = change(node, source, kotlin) {
        let text = |n: Node| source.get(n.byte_range()).unwrap_or("").to_owned();
        out.entry(line_of(starts, node.start_byte()))
            .or_default()
            .push((text(node), change));
        // An `else if`, and any Kotlin `if`, may be recorded by its
        // condition alone.
        if let Some(c) = tested(node) {
            out.entry(line_of(starts, c.start_byte()))
                .or_default()
                .push((text(c), Some(Change::Invert)));
        }
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, starts, kotlin, out);
    }
}

/// `Some(change)` for a statement (`Some(None)`: not assessed).
fn change(node: Node, source: &str, kotlin: bool) -> Option<Option<Change>> {
    let text = source.get(node.byte_range()).unwrap_or("");
    // Returning null or assigning null, changed to null, is the same code: a
    // question no test could answer yes to.
    let statement = text.trim().trim_end_matches(';').trim();
    if statement == "return null"
        || matches!(
            node.kind(),
            "local_variable_declaration"
                | "expression_statement"
                | "assignment"
                | "property_declaration"
        ) && statement.rsplit_once('=').is_some_and(|(head, value)| {
            value.trim() == "null" && !head.ends_with(['=', '!', '<', '>'])
        })
    {
        return Some(Some(Change::Skip));
    }
    let has_value = || {
        let first = text.split('\n').next().unwrap_or("");
        super::split_initializer(first).is_some()
    };
    Some(match node.kind() {
        "line_comment" | "block_comment" | "comment" | "multiline_comment" => return None,
        "if_statement" | "if_expression" => Some(Change::Invert),
        "return_statement" | "return_expression" => Some(if node.named_child_count() > 0 {
            Change::ReturnUndefined
        } else {
            Change::Skip
        }),
        "local_variable_declaration" => {
            let mut cursor = node.walk();
            let valued = node.named_children(&mut cursor).any(|d| {
                d.kind() == "variable_declarator" && d.child_by_field_name("value").is_some()
            });
            valued.then_some(Change::ValueUndefined)
        }
        "property_declaration" => {
            (has_value() && !text.contains(" by ")).then_some(Change::ValueUndefined)
        }
        // Kotlin's grammar has no expression statement.
        "expression_statement" => {
            let assignment = node
                .named_child(0)
                .filter(|c| c.kind() == "assignment_expression");
            match assignment.and_then(|a| a.child_by_field_name("operator")) {
                Some(op) if source.get(op.byte_range()) == Some("=") => {
                    Some(Change::ValueUndefined)
                }
                _ => Some(Change::Skip),
            }
        }
        "assignment" => {
            let plain = text
                .split('\n')
                .next()
                .and_then(super::split_initializer)
                .is_some_and(|head| {
                    !head
                        .trim_end_matches('=')
                        .ends_with(['+', '-', '*', '/', '%'])
                });
            Some(if plain {
                Change::ValueUndefined
            } else {
                Change::Skip
            })
        }
        "local_class_declaration"
        | "class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "record_declaration"
        | "function_declaration"
        | "object_declaration"
        | "type_alias" => None,
        // A Kotlin branch's last expression is the value the branch yields
        // when its `if`, `when` or `try` is used as one: skipping it would
        // not compile, and the change that means something is the value.
        _ if kotlin && yields_value(node) => Some(Change::ReturnUndefined),
        // A `when`, `switch` or `try` that ends a function with a result
        // returns from inside it: skipping it would leave the function
        // without one, so what changes is the value it returns.
        "when_expression"
        | "try_expression"
        | "switch_expression"
        | "try_statement"
        | "try_with_resources_statement"
            if ends_a_function_with_a_result(node, source) =>
        {
            Some(Change::ReturnUndefined)
        }
        // A Kotlin `throw` the function would otherwise end on: not
        // throwing leaves it nothing to return, so it returns the default.
        // Java's grammar has no throw expression.
        "throw_expression" if on_final_path(node, source) => Some(Change::ReturnUndefined),
        _ => Some(Change::Skip),
    })
}

/// Whether a statement is the last of a function body that has a result
/// type other than `Unit` or `void`.
fn ends_a_function_with_a_result(node: Node, source: &str) -> bool {
    let Some(block) = node.parent().filter(|p| p.kind() == "block") else {
        return false;
    };
    let mut cursor = block.walk();
    let last = block
        .named_children(&mut cursor)
        .filter(|child| !child.kind().contains("comment"))
        .last();
    if last.map(|last| last.id()) != Some(node.id()) {
        return false;
    }
    // Kotlin: block <- function_body <- function_declaration; Java:
    // block <- method_declaration.
    let function = match block.parent() {
        Some(p) if p.kind() == "function_body" => p.parent(),
        Some(p) if p.kind() == "method_declaration" => Some(p),
        _ => None,
    };
    let Some(function) = function else {
        return false;
    };
    if function.kind() == "method_declaration" {
        return function
            .child_by_field_name("type")
            .is_some_and(|t| t.kind() != "void_type");
    }
    let mut cursor = function.walk();
    let result = function
        .named_children(&mut cursor)
        .find(|c| c.kind().ends_with("_type"));
    result.is_some_and(|t| {
        source
            .get(t.byte_range())
            .is_some_and(|text| text.trim() != "Unit")
    })
}

/// Whether a statement is where its function ends when it completes: the
/// last of its block, whose `if`, `when`, `try` or `catch` is in turn the
/// last of its block, up to the body of a function with a result.
fn on_final_path(node: Node, source: &str) -> bool {
    let mut node = node;
    loop {
        if ends_a_function_with_a_result(node, source) {
            return true;
        }
        let Some(block) = node.parent().filter(|p| p.kind() == "block") else {
            return false;
        };
        let mut cursor = block.walk();
        let last = block
            .named_children(&mut cursor)
            .filter(|child| !child.kind().contains("comment"))
            .last();
        if last.map(|last| last.id()) != Some(node.id()) {
            return false;
        }
        node = match block.parent() {
            Some(p) if matches!(p.kind(), "if_expression" | "try_expression") => p,
            Some(p) if matches!(p.kind(), "catch_block" | "when_entry" | "finally_block") => {
                p.parent().expect(OWNED)
            }
            _ => return false,
        };
    }
}

/// Whether a statement is the last in a block whose value is used: a branch
/// of an `if`, `when` or `try` that is itself a value.
fn yields_value(node: Node) -> bool {
    let Some(block) = node.parent().filter(|p| p.kind() == "block") else {
        return false;
    };
    let mut cursor = block.walk();
    let last = block
        .named_children(&mut cursor)
        .filter(|child| !child.kind().contains("comment"))
        .last();
    if last.map(|last| last.id()) != Some(node.id()) {
        return false;
    }
    let owner = match block.parent() {
        Some(p) if matches!(p.kind(), "if_expression" | "try_expression") => p,
        Some(p) if matches!(p.kind(), "when_entry" | "catch_block") => p.parent().expect(OWNED),
        _ => return false,
    };
    used(owner)
}

/// Whether an `if`, `when` or `try` is used for its value.
fn used(expression: Node) -> bool {
    let Some(parent) = expression.parent() else {
        return false;
    };
    match parent.kind() {
        // A statement of its own, unless it ends a block that is a value.
        "block" => yields_value(expression),
        "statements" | "source_file" | "class_body" => false,
        // The lambda's value, when it ends the lambda and the lambda's value
        // is taken.
        "lambda_literal" => ends(parent, expression) && lambda_value_used(parent),
        // `else if`: the outer `if` decides.
        "if_expression" => used(parent),
        "when_entry" => parent.parent().is_some_and(used),
        // Assigned, returned, passed, an expression body, an operand.
        _ => true,
    }
}

/// Whether `node` is the last of `body`'s statements, comments aside.
fn ends(body: Node, node: Node) -> bool {
    let mut cursor = body.walk();
    body.named_children(&mut cursor)
        .filter(|child| !child.kind().contains("comment"))
        .last()
        .is_some_and(|last| last.id() == node.id())
}

/// Whether what a lambda returns is used. A trailing lambda's result is its
/// call's to use (`val x = run { .. }`, not `run { .. }` on its own); any
/// other lambda is a value, whose result is taken wherever it is taken.
fn lambda_value_used(lambda: Node) -> bool {
    match lambda.parent() {
        Some(trailing) if trailing.kind() == "annotated_lambda" => trailing
            .parent()
            .filter(|call| call.kind() == "call_expression")
            .is_some_and(used),
        _ => used(lambda),
    }
}

/// `if (c) {`, `} else if (c) {` -> `c`
pub(super) fn condition(line: &str) -> String {
    super::condition(line)
}

/// A test by method name (`pkg.Class#method(String)` or a Kotlin display
/// name), with its annotations and class line.
pub(super) fn locate(lines: &[&str], name: &str) -> Option<Located> {
    // `pkg.Class#method(String)#[3] display`: the method is the segment with
    // its parameter list; an invocation of it follows.
    let method = name
        .split('#')
        .skip(1)
        .find(|segment| segment.contains('('))
        .or_else(|| name.rsplit_once('#').map(|(_, m)| m))
        .unwrap_or(name);
    let method = method.split('(').next().unwrap_or(method).trim();
    if method.is_empty() {
        return None;
    }
    // `Outer$Inner#method`: JUnit's `@Nested` classes reuse method names, so
    // the method is looked for inside the class the name gives. A numbered
    // segment is an anonymous class and names nothing to look for.
    let class = name.split('#').next().unwrap_or(name);
    let (mut from, mut to) = (0, lines.len());
    for nested in class
        .split('$')
        .skip(1)
        .filter(|part| !part.is_empty() && !part.bytes().all(|b| b.is_ascii_digit()))
    {
        let declares = |line: &str| {
            ["class ", "object "].iter().any(|keyword| {
                line.match_indices(&format!("{keyword}{nested}"))
                    .any(|(at, found)| {
                        (at == 0 || !line.as_bytes()[at - 1].is_ascii_alphanumeric())
                            && line[at + found.len()..]
                                .chars()
                                .next()
                                .is_none_or(|c| !(c.is_alphanumeric() || c == '_'))
                    })
            })
        };
        let Some(at) = (from..to).find(|&i| declares(lines[i])) else {
            break;
        };
        (from, to) = (at, (super::block_end(lines, at) + 1).min(lines.len()));
    }
    let declares_method = |i: &usize| {
        let t = lines[*i].trim_start();
        t.contains(&format!("void {method}("))
            || t.contains(&format!("fun {method}("))
            || t.contains(&format!("fun `{method}`("))
    };
    // A class longer than a body is read is searched past its first lines,
    // then the whole file, rather than not at all.
    let start = (from..to)
        .find(declares_method)
        .or_else(|| (from..lines.len()).find(declares_method))
        .or_else(|| (0..lines.len()).find(declares_method))?;
    let indent = super::indent(lines[start]);
    // The annotations above it, an argument list spread over lines included:
    // walking up, a line is taken while an annotation's parentheses are open.
    let mut first = start;
    let mut open = 0i64;
    while first > 0 {
        let t = lines[first - 1].trim_start();
        if !(t.starts_with('@') || t.starts_with(')') || open > 0) {
            break;
        }
        open += t.matches(')').count() as i64 - t.matches('(').count() as i64;
        first -= 1;
    }
    let class = (0..first).rev().find(|&i| {
        let t = lines[i];
        super::indent(t) < indent
            && [" class ", " object ", "class ", "object "]
                .iter()
                .any(|k| t.trim_start().starts_with(k.trim_start()) || t.contains(k))
    });
    Some(Located {
        context: class.into_iter().chain(first..start).collect(),
        start,
        end: super::brace_end(lines, start),
    })
}

/// `pkg.Outer$Inner#method()` -> the file suffixes that may hold it.
pub(super) fn class_paths(name: &str) -> Vec<String> {
    let class = name.split('#').next().unwrap_or(name);
    let class = class.split('$').next().unwrap_or(class);
    let base = class.replace('.', "/");
    vec![format!("{base}.java"), format!("{base}.kt")]
}

/// A JVM test's helpers: test-side classes it imports.
pub(super) fn imports(text: &str) -> Vec<String> {
    text.split('\n')
        .filter_map(|l| l.trim_start().strip_prefix("import "))
        .filter(|l| !l.starts_with("static "))
        .map(|l| l.trim_end_matches(';').trim().replace('.', "/"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `@Nested` classes reuse method names: each test is found in its own.
    #[test]
    fn a_nested_test_is_found_in_its_own_class() {
        let text = "class CartTest {\n  @Nested\n  class WhenEmpty {\n    @Test\n    void total() {\n      assertEquals(0, cart.total());\n    }\n  }\n\n  @Nested\n  class WithItems {\n    @Test\n    void total() {\n      assertEquals(3, cart.total());\n    }\n  }\n}\n";
        let lines = text.split('\n').collect::<Vec<_>>();
        let at = |name: &str| locate(&lines, name).map(|found| found.start + 1);
        assert_eq!(at("app.CartTest$WithItems#total()"), Some(13));
        assert_eq!(at("app.CartTest$WhenEmpty#total()"), Some(5));
        // Without a nested class, or with an anonymous one, the first.
        assert_eq!(at("app.CartTest#total()"), Some(5));
        assert_eq!(at("app.CartTest$1#total()"), Some(5));
        // A name with no parameter list names its method all the same; one
        // the class does not declare is not found.
        assert_eq!(at("app.CartTest$WithItems#total"), Some(13));
        assert_eq!(at("app.CartTest#missing()"), None);
        let kotlin = "class CartSpec {\n    inner class Empty {\n        @Test\n        fun `has no total`() {}\n    }\n    inner class EmptyOther {\n        @Test\n        fun `has no total`() {}\n    }\n}\n";
        let lines = kotlin.split('\n').collect::<Vec<_>>();
        assert_eq!(
            locate(&lines, "app.CartSpec$EmptyOther#has no total()").map(|found| found.start + 1),
            Some(8)
        );
    }
    #[test]
    fn a_test_name_finds_its_method_by_whole_names_only() {
        let text = "class T {\n  class MyInner {\n    void run() {}\n  }\n  class Inner_x {\n    void run() {}\n  }\n  class Inner {\n    @Tag(\n      \"slow\"\n    )\n    void run() {}\n  }\n  fun plain() {}\n}\n";
        let lines = text.split('\n').collect::<Vec<_>>();
        let at =
            |name: &str| locate(&lines, name).map(|found| (found.start + 1, found.context.len()));
        // `Inner` is a whole class name: not `MyInner`, not `Inner_x`.
        assert_eq!(at("app.T$Inner#run()").map(|(line, _)| line), Some(12));
        // An empty segment names no class, and the first method is taken.
        assert_eq!(at("app.T$$Inner#run()").map(|(line, _)| line), Some(12));
        assert_eq!(at("app.T#plain()").map(|(line, _)| line), Some(14));
        // An annotation whose arguments close on their own line belongs to it.
        assert!(at("app.T$Inner#run()").unwrap().1 >= 4);
        assert_eq!(at("app.T#()"), None);
        assert_eq!(at("app.T#missing()"), None);
        // A method on the file's first line has nothing above it.
        let first = ["void solo() {}", ""];
        assert_eq!(
            locate(&first, "app.S#solo()").map(|found| found.start),
            Some(0)
        );
    }
    use crate::assertion_coverage::{Language, change_at};

    /// A Kotlin file the instrumenter cannot parse is still read; a comment
    /// is no statement; a bare `return` and a delegated property change
    /// nothing; a statement before the last ends no function; and a `throw`
    /// ends its function only on the path the function ends on.
    #[test]
    fn statements_off_the_final_path_and_in_broken_files_are_read() {
        let j = Language::Jvm;
        let broken = statement_starts(
            "A.kt",
            "fun f(): Int {\n    val x by lazy { 1 }\n    return\n    val y = (\n}\n",
        );
        assert_eq!(change_at(&broken, 2, "val x by lazy { 1 }", j), None);
        assert_eq!(change_at(&broken, 3, "return", j), Some(Change::Skip));
        let java = statement_starts(
            "A.java",
            "class A {\n    int f() {\n        // c\n        try { g(); } finally { h(); }\n        return;\n    }\n}\n",
        );
        assert!(!java.contains_key(&3), "{java:?}");
        assert_eq!(
            change_at(&java, 4, "try { g(); } finally { h(); }", j),
            Some(Change::Skip)
        );
        assert_eq!(change_at(&java, 5, "return;", j), Some(Change::Skip));
        let kotlin = statement_starts(
            "A.kt",
            "fun k(a: Boolean): Int {\n    if (a) {\n        throw E()\n    } else {\n        g()\n    }\n    return 1\n}\nfun m(a: Boolean): Int {\n    try {\n        g()\n        throw E()\n    } catch (e: E) {\n        throw E()\n    }\n}\nfun h() {\n    throw E()\n    g()\n}\n",
        );
        assert_eq!(change_at(&kotlin, 3, "throw E()", j), Some(Change::Skip));
        assert_eq!(
            change_at(&kotlin, 12, "throw E()", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(
            change_at(&kotlin, 14, "throw E()", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(change_at(&kotlin, 18, "throw E()", j), Some(Change::Skip));
    }

    /// A class is found by its whole name at the start of a line, and not
    /// inside a longer word or before more of a name.
    #[test]
    fn a_nested_class_is_named_by_its_whole_word_anywhere_on_its_line() {
        let text = "class T {\nsubclass Inner {\n}\nclass Inner2 {\n    void run() {}\n}\nclass Inner {\n    void run() {}\n}\n}\n";
        let lines = text.split('\n').collect::<Vec<_>>();
        assert_eq!(
            locate(&lines, "app.T$Inner#run()").map(|found| found.start + 1),
            Some(8)
        );
    }

    #[test]
    fn null_changed_to_null_is_not_asked() {
        let source = "class A {\n    String f(String x) {\n        String y = null;\n        x = null;\n        if (x == null) {\n            return null;\n        }\n        return y;\n    }\n}\n";
        let starts = statement_starts("A.java", source);
        let j = Language::Jvm;
        assert_eq!(
            change_at(&starts, 3, "String y = null;", j),
            Some(Change::Skip)
        );
        assert_eq!(change_at(&starts, 4, "x = null;", j), Some(Change::Skip));
        assert_eq!(change_at(&starts, 6, "return null;", j), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 8, "return y;", j),
            Some(Change::ReturnUndefined)
        );
    }

    /// A comparison with null is a value, not null assigned; a Kotlin
    /// property delegated with `by` has no value of its own to change.
    #[test]
    fn null_comparisons_and_delegated_properties_are_told_apart() {
        let java = "class A {\n    boolean f(String x) {\n        boolean b = x != null;\n        boolean c = x == null;\n        return b;\n    }\n}\n";
        let starts = statement_starts("A.java", java);
        let j = Language::Jvm;
        assert_eq!(
            change_at(&starts, 3, "boolean b = x != null;", j),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 4, "boolean c = x == null;", j),
            Some(Change::ValueUndefined)
        );
        let kotlin = "fun k() {\n    val lazyOne by lazy { 1 }\n    val plain: Int = 2\n    var none: String? = null\n    var seen by observable(0) { _, _, new -> last = new }\n}\n";
        let starts = statement_starts("K.kt", kotlin);
        assert_eq!(change_at(&starts, 2, "val lazyOne by lazy { 1 }", j), None);
        // A delegate whose lambda assigns is still no value.
        assert_eq!(
            change_at(&starts, 5, "var seen by observable(0) {", j),
            None
        );
        assert_eq!(
            change_at(&starts, 3, "val plain: Int = 2", j),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 4, "var none: String? = null", j),
            Some(Change::Skip)
        );
    }

    #[test]
    fn java_changes_follow_the_audit() {
        let source = "class A {\n    int f(int x) {\n        int y;\n        int z = x + 1;\n        if (z > 2) {\n            return z;\n        } else if (z < 0) {\n            z += 1;\n        }\n        y = z;\n        log(z);\n        return y;\n    }\n}\n";
        let starts = statement_starts("A.java", source);
        let j = Language::Jvm;
        assert_eq!(change_at(&starts, 3, "int y;", j), None);
        assert_eq!(
            change_at(&starts, 4, "int z = x + 1;", j),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 5, "if (z > 2) {", j),
            Some(Change::Invert)
        );
        assert_eq!(
            change_at(&starts, 6, "return z;", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(change_at(&starts, 7, "z < 0", j), Some(Change::Invert));
        assert_eq!(change_at(&starts, 8, "z += 1;", j), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 10, "y = z;", j),
            Some(Change::ValueUndefined)
        );
        assert_eq!(change_at(&starts, 11, "log(z);", j), Some(Change::Skip));
        // A class declared in a method is no statement to change.
        let local = "class A {\n    void f() {\n        class L {}\n    }\n}\n";
        let starts = statement_starts("A.java", local);
        assert_eq!(change_at(&starts, 3, "class L {}", j), None);
        // A Kotlin `else if` branch's value is the outer `if`'s; an `if`
        // ending a lambda whose value nobody takes yields nothing.
        let kotlin = "fun f(c: Boolean, d: Boolean): Int {\n    val v = if (c) {\n        1\n    } else if (d) {\n        log()\n        2\n    } else {\n        3\n    }\n    run {\n        if (c) {\n            log()\n            4\n        } else {\n            5\n        }\n    }\n    return v\n}\n";
        let starts = statement_starts("F.kt", kotlin);
        assert_eq!(change_at(&starts, 6, "2", j), Some(Change::ReturnUndefined));
        assert_eq!(change_at(&starts, 13, "4", j), Some(Change::Skip));
        // One whose value is taken, by the call it trails or as the lambda's
        // own value, is a value; one that does not end its lambda is not.
        let taken = concat!(
            "fun g(c: Boolean): Int {\n",
            "    val x = run {\n",
            "        if (c) {\n",
            "            log()\n",
            "            4\n",
            "        } else {\n",
            "            5\n",
            "        }\n",
            "    }\n",
            "    val f: () -> Int = {\n",
            "        if (c) {\n",
            "            log()\n",
            "            6\n",
            "        } else {\n",
            "            7\n",
            "        }\n",
            "    }\n",
            "    val y = run {\n",
            "        if (c) {\n",
            "            log()\n",
            "            8\n",
            "        } else {\n",
            "            9\n",
            "        }\n",
            "        10\n",
            "    }\n",
            "    return x + f() + y\n",
            "}\n",
        );
        let starts = statement_starts("G.kt", taken);
        assert_eq!(change_at(&starts, 5, "4", j), Some(Change::ReturnUndefined));
        assert_eq!(
            change_at(&starts, 13, "6", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(change_at(&starts, 21, "8", j), Some(Change::Skip));
    }

    /// Forge's `stringBuilder.toString()` ending a `when` branch: the value
    /// the branch yields, which skipping would not even compile.
    #[test]
    fn a_kotlin_branch_value_is_changed_as_a_value() {
        let source = "fun f(c: Boolean, j: Any): String {\n    val y = if (c) {\n        log()\n        \"a\"\n    } else {\n        \"b\"\n    }\n    if (c) {\n        log()\n    }\n    return when (j) {\n        is Int -> {\n            log()\n            j.toString()\n        }\n        else -> try {\n            g(j)\n        } catch (e: Exception) {\n            y\n        }\n    }\n}\n";
        let starts = statement_starts("F.kt", source);
        let j = Language::Jvm;
        assert_eq!(change_at(&starts, 3, "log()", j), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 4, "\"a\"", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(
            change_at(&starts, 6, "\"b\"", j),
            Some(Change::ReturnUndefined)
        );
        // An `if` that is a statement yields nothing.
        assert_eq!(change_at(&starts, 9, "log()", j), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 14, "j.toString()", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(
            change_at(&starts, 17, "g(j)", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(
            change_at(&starts, 19, "y", j),
            Some(Change::ReturnUndefined)
        );
    }

    /// Forge's `parse`: a `when` that returns from every branch ends the
    /// function, and skipping it would not compile.
    #[test]
    fn a_when_that_ends_a_function_changes_what_it_returns() {
        let kotlin = "fun parse(json: Any): JSON {\n    when (json) {\n        is Int -> return a(json)\n        else -> return b()\n    }\n}\nfun log(json: Any) {\n    when (json) {\n        is Int -> println(json)\n    }\n}\nfun unit(json: Any): Unit {\n    when (json) {\n        is Int -> return\n    }\n}\n";
        let starts = statement_starts("F.kt", kotlin);
        let j = Language::Jvm;
        assert_eq!(
            change_at(&starts, 2, "when (json) {", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(
            change_at(&starts, 8, "when (json) {", j),
            Some(Change::Skip)
        );
        assert_eq!(
            change_at(&starts, 13, "when (json) {", j),
            Some(Change::Skip)
        );
        // moshi's `nextName` and `Moshi.adapter`: a `throw` the function
        // would end on, directly or from a `catch`.
        let throws = "fun name(): String {\n    if (a) {\n        return b\n    }\n    throw error(\"x\")\n}\nfun adapter(): A {\n    try {\n        return c()\n    } catch (e: Exception) {\n        throw wrap(e)\n    }\n}\nfun check() {\n    throw error(\"y\")\n}\n";
        let starts = statement_starts("T.kt", throws);
        assert_eq!(
            change_at(&starts, 5, "throw error(\"x\")", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(
            change_at(&starts, 11, "throw wrap(e)", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(
            change_at(&starts, 15, "throw error(\"y\")", j),
            Some(Change::Skip)
        );
        let java = "class A {\n    int f(int x) {\n        switch (x) {\n            case 1: return 2;\n            default: return 3;\n        }\n    }\n    void g(int x) {\n        switch (x) {\n            default: h();\n        }\n    }\n}\n";
        let starts = statement_starts("A.java", java);
        assert_eq!(
            change_at(&starts, 3, "switch (x) {", j),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(change_at(&starts, 9, "switch (x) {", j), Some(Change::Skip));
    }

    #[test]
    fn kotlin_changes_and_tests() {
        let source = "object C {\n    fun f(a: Int): String {\n        val limit = 10\n        var label = \"small\"\n        if (a > limit) {\n            label = \"big\"\n        }\n        label += \"!\"\n        return label\n    }\n}\n";
        let starts = statement_starts("C.kt", source);
        let j = Language::Jvm;
        assert_eq!(
            change_at(&starts, 3, "val limit = 10", j),
            Some(Change::ValueUndefined)
        );
        assert_eq!(change_at(&starts, 5, "a > limit", j), Some(Change::Invert));
        assert_eq!(
            change_at(&starts, 6, "label = \"big\"", j),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 8, "label += \"!\"", j),
            Some(Change::Skip)
        );
        assert_eq!(
            change_at(&starts, 9, "return label", j),
            Some(Change::ReturnUndefined)
        );
        let suite = "class CTest {\n    @Test\n    fun `big when large`() {\n        assertEquals(\"big\", C.f(20))\n    }\n}\n";
        let lines = suite.split('\n').collect::<Vec<_>>();
        let found = locate(&lines, "app.CTest#big when large()").unwrap();
        let invocation = locate(&lines, "app.CTest#big when large()#[2] big (x)").unwrap();
        assert_eq!(invocation.start, 2);
        assert_eq!(
            (found.context.clone(), found.start, found.end),
            (vec![0, 1], 2, 4)
        );
        assert_eq!(
            class_paths("app.Outer$Inner#m()"),
            vec!["app/Outer.java", "app/Outer.kt"]
        );
    }
    /// What each Kotlin and Java statement shape is asked as. A class-level
    /// property and an expression-bodied function are not statements here, so
    /// nothing is asked of them.
    #[test]
    fn each_jvm_statement_shape_is_asked_its_own_way() {
        let kotlin = "class K {\n    val lazyOne by lazy { 1 }\n    val plain: Int = 2\n    fun f(a: Boolean): Int {\n        val x: Int\n        x = 3\n        val y = if (a) 1 else 2\n        val z = try { 4 } catch (e: Exception) { 5 }\n        var w: String? = null\n        w = \"v\"\n        if (a) return 6\n        return if (a) x else y\n    }\n    fun g(a: Boolean) = if (a) 7 else 8\n    fun h(a: Boolean): Int {\n        return try { 9 } finally { }\n    }\n}\n";
        let starts = statement_starts("K.kt", kotlin);
        let first = |line: usize| starts.get(&line).map(|entries| entries[0].1);
        assert_eq!(first(2), None);
        assert_eq!(first(5), Some(None), "a declaration with no value");
        assert_eq!(first(6), Some(Some(Change::ValueUndefined)));
        assert_eq!(first(7), Some(Some(Change::ValueUndefined)));
        assert_eq!(starts[&8].len(), 3, "the try value and each branch's");
        assert_eq!(
            first(9),
            Some(Some(Change::Skip)),
            "null is already the other value"
        );
        assert_eq!(first(11), Some(Some(Change::Invert)));
        assert_eq!(first(12), Some(Some(Change::ReturnUndefined)));
        assert_eq!(first(14), None);
        assert_eq!(first(16), Some(Some(Change::ReturnUndefined)));
        assert!(statement_starts("build.gradle.kts", "val v = 1\nprintln(v)\n").is_empty());
        let java = "class J {\n    int f(boolean a) {\n        int x;\n        x = 1;\n        if (a) x = 2;\n        String s = a ? \"t\" : null;\n        Object o = null;\n        if (o != null) { return 3; }\n        return x;\n    }\n}\n";
        let starts = statement_starts("J.java", java);
        let first = |line: usize| starts.get(&line).map(|entries| entries[0].1);
        assert_eq!(first(3), Some(None));
        assert_eq!(first(4), Some(Some(Change::ValueUndefined)));
        assert_eq!(first(5), Some(Some(Change::Invert)));
        assert_eq!(first(6), Some(Some(Change::ValueUndefined)));
        assert_eq!(first(7), Some(Some(Change::Skip)));
        assert_eq!(starts[&8].len(), 3);
        assert_eq!(first(9), Some(Some(Change::ReturnUndefined)));
    }
}
