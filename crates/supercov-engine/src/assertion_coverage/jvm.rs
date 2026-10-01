//! Java and Kotlin: statements by tree-sitter, tests by class and method name.
use super::{Change, Located, Starts, line_of, line_starts};
use crate::jvm_instrumenter::JvmLanguage;
use tree_sitter::{Node, Parser};

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
        Err(_) => {
            let mut parser = Parser::new();
            let grammar = if kotlin(file) {
                tree_sitter_kotlin_ng::LANGUAGE.into()
            } else {
                tree_sitter_java::LANGUAGE.into()
            };
            if parser.set_language(&grammar).is_err() {
                return out;
            }
            let Some(tree) = parser.parse(source, None) else {
                return out;
            };
            tree
        }
    };
    let starts = line_starts(source);
    walk(tree.root_node(), source, &starts, kotlin(file), &mut out);
    out
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
        if matches!(node.kind(), "if_statement" | "if_expression")
            && let Some(c) = node.child_by_field_name("condition")
        {
            let c = if c.kind() == "parenthesized_expression" {
                c.named_child(0).unwrap_or(c)
            } else {
                c
            };
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
        "expression_statement" if !kotlin => {
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
        _ => Some(Change::Skip),
    })
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
        Some(p) if matches!(p.kind(), "when_entry" | "catch_block") => match p.parent() {
            Some(owner) => owner,
            None => return false,
        },
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
        "statements" | "source_file" | "class_body" | "lambda_literal" => false,
        // `else if`: the outer `if` decides.
        "if_expression" => used(parent),
        "when_entry" => parent.parent().is_some_and(used),
        // Assigned, returned, passed, an expression body, an operand.
        _ => true,
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
    let start = lines.iter().position(|l| {
        let t = l.trim_start();
        t.contains(&format!("void {method}("))
            || t.contains(&format!("fun {method}("))
            || t.contains(&format!("fun `{method}`("))
    })?;
    let indent = super::indent(lines[start]);
    let mut first = start;
    while first > 0 && {
        let t = lines[first - 1].trim_start();
        t.starts_with('@') || t.starts_with(')')
    } {
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
    use crate::assertion_coverage::{Language, change_at};

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
}
