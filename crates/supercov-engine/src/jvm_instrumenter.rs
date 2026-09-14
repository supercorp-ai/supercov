//! Supercov-owned Java and Kotlin parsing.

use tree_sitter::Parser;

pub fn parse_java(source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_java::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

pub fn parse_kotlin(source: &str) -> Option<tree_sitter::Tree> {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_kotlin_ng::LANGUAGE.into())
        .ok()?;
    parser.parse(source, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_jvm_grammars_parse_the_shapes_an_instrumenter_needs() {
        // Java and Kotlin differ in ways that matter to a rewriter, and the
        // differences are worth pinning: Java's statements are direct children
        // of a block and its `if` condition is parenthesised, while Kotlin's
        // `if` is an expression whose condition is bare.
        let java = parse_java(JAVA).expect("java parses");
        assert!(!java.root_node().has_error());
        assert!(kinds(java.root_node()).contains("method_declaration"));
        assert!(kinds(java.root_node()).contains("if_statement"));
        assert!(kinds(java.root_node()).contains("switch_expression"));

        let kotlin = parse_kotlin(KOTLIN).expect("kotlin parses");
        assert!(!kotlin.root_node().has_error());
        let shapes = kinds(kotlin.root_node());
        assert!(shapes.contains("function_declaration"));
        assert!(
            shapes.contains("if_expression"),
            "Kotlin's if is an expression"
        );
        assert!(shapes.contains("when_expression"));
    }

    fn kinds(node: tree_sitter::Node) -> std::collections::BTreeSet<String> {
        let mut out = std::collections::BTreeSet::new();
        out.insert(node.kind().to_owned());
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if child.is_named() {
                out.extend(kinds(child));
            }
        }
        out
    }

    const JAVA: &str = r#"package app;

class Classify {
    String classify(int a, boolean b) {
        if (a > 10 && b) {
            return "big";
        }
        for (int i = 0; i < a; i++) {
            System.out.print(i);
        }
        switch (a) {
            case 0: return "zero";
            default: return "small";
        }
    }
}
"#;

    const KOTLIN: &str = r#"package app

fun classify(a: Int, b: Boolean): String {
    if (a > 10 && b) {
        return "big"
    }
    for (i in 0 until a) {
        println(i)
    }
    return when (a) {
        0 -> "zero"
        else -> "small"
    }
}
"#;
}
