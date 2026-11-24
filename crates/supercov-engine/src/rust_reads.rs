//! What a Rust file names by path, for `tests affected`: its string literals
//! and where each sits, the test functions and macro entries that hold them,
//! the files it embeds with `include_str!`, `include_bytes!` or `include!`,
//! and the module files it declares.
//!
//! A Rust test reaches a fixture, a script or a data file by naming it, never
//! by importing it, so a helper changing used to reach every Rust test: no
//! record and no import said which one loads it. What a test names says.
use ra_ap_syntax::{
    AstNode, AstToken, Edition, NodeOrToken, SourceFile, SyntaxKind, SyntaxNode, SyntaxToken,
    ast::{self, HasAttrs, HasName},
};

/// One function of a file: its path from the file's top level
/// (`tests::it_works`), the bytes it spans, and whether it is a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Function {
    pub path: String,
    pub range: (usize, usize),
    pub test: bool,
}

/// A file embedded by a macro: where the macro sits, the path it names
/// (relative to the file), and whether that path is only the start of one
/// (`concat!("assets/", $name)`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Embed {
    pub offset: usize,
    pub path: String,
    pub prefix: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    /// Each string literal's value and the byte it starts at.
    pub strings: Vec<(usize, String)>,
    pub functions: Vec<Function>,
    /// An entry of a macro invocation that starts with an identifier, such as
    /// `python => "python-integration.mjs"` in a macro that declares a test
    /// per entry: the identifier and the bytes of the entry.
    pub entries: Vec<(String, (usize, usize))>,
    pub embeds: Vec<Embed>,
    /// `mod name;`: the modules declared in a file of their own.
    pub modules: Vec<String>,
    /// The paths of `#[cfg(test)]` modules: test code, which no probe
    /// measures.
    pub test_modules: Vec<String>,
}

impl Outline {
    /// The literals in `range`.
    pub fn strings_in(&self, range: (usize, usize)) -> impl Iterator<Item = &str> {
        self.strings
            .iter()
            .filter(move |(offset, _)| range.0 <= *offset && *offset < range.1)
            .map(|(_, text)| text.as_str())
    }
}

/// A node's path from the file's top level through the modules around it,
/// its own name last.
fn module_path(node: &SyntaxNode) -> String {
    let mut path = node
        .ancestors()
        .filter_map(|ancestor| {
            ast::Module::cast(ancestor.clone())
                .and_then(|module| module.name())
                .or_else(|| ast::Fn::cast(ancestor).and_then(|function| function.name()))
        })
        .map(|name| name.text().to_string())
        .collect::<Vec<_>>();
    path.reverse();
    path.join("::")
}

fn span(node: &SyntaxNode) -> (usize, usize) {
    let range = node.text_range();
    (
        u32::from(range.start()) as usize,
        u32::from(range.end()) as usize,
    )
}

/// The file's outline; `None` where it does not parse.
pub fn outline(source: &str) -> Option<Outline> {
    let parsed = SourceFile::parse(source, Edition::Edition2024);
    if !parsed.errors().is_empty() {
        return None;
    }
    let mut outline = Outline::default();
    for element in parsed.tree().syntax().descendants_with_tokens() {
        match element {
            NodeOrToken::Token(token) => {
                if token.kind() == SyntaxKind::IDENT
                    && matches!(token.text(), "include_str" | "include_bytes" | "include")
                {
                    outline.embeds.extend(embed(&token));
                }
                // A literal inside a macro's tokens is a bare token too.
                if let Some(text) = ast::String::cast(token.clone())
                    .and_then(|string| string.value().ok().map(|value| value.into_owned()))
                {
                    outline
                        .strings
                        .push((u32::from(token.text_range().start()) as usize, text));
                }
            }
            NodeOrToken::Node(node) => {
                if let Some(function) = ast::Fn::cast(node.clone()) {
                    outline.functions.push(Function {
                        path: module_path(&node),
                        range: span(&node),
                        test: function.attrs().any(|attribute| {
                            attribute
                                .path()
                                .and_then(|path| path.segment())
                                .is_some_and(|segment| segment.to_string() == "test")
                        }),
                    });
                } else if let Some(module) = ast::Module::cast(node.clone()) {
                    if module.attrs().any(|attribute| {
                        attribute.syntax().text().to_string().replace(' ', "") == "#[cfg(test)]"
                    }) {
                        outline.test_modules.push(module_path(&node));
                    }
                    if module.item_list().is_none() {
                        outline
                            .modules
                            .extend(module.name().map(|name| name.text().to_string()));
                    }
                } else if let Some(call) = ast::MacroCall::cast(node.clone()) {
                    read_macro(&call, &mut outline);
                }
            }
        }
    }
    Some(outline)
}

fn literal(element: &NodeOrToken<SyntaxNode, SyntaxToken>) -> Option<String> {
    element
        .as_token()
        .cloned()
        .and_then(ast::String::cast)
        .and_then(|string| string.value().ok().map(|value| value.into_owned()))
}

/// The file an `include*!` names: `include_str!("x")`, or
/// `include_bytes!(concat!("dir/", $name))`, whose first literal is where
/// every file it can name starts. Read from the tokens, so an include inside
/// a `macro_rules!` body counts. A path computed any other way (`env!`) is a
/// build's output, not a file of the checkout.
fn embed(name: &SyntaxToken) -> Option<Embed> {
    // Parsed, the name sits in the path of a macro call; in a macro's own
    // tokens, it is followed by `!` and the arguments.
    let tree = match name.parent_ancestors().find_map(ast::MacroCall::cast) {
        Some(call)
            if call.path().is_some_and(|path| {
                path.syntax().text_range().contains_range(name.text_range())
            }) =>
        {
            call.token_tree()?.syntax().clone()
        }
        _ => {
            let mut after =
                std::iter::successors(name.next_sibling_or_token(), |e| e.next_sibling_or_token())
                    .filter(|e| !matches!(e.kind(), SyntaxKind::WHITESPACE | SyntaxKind::COMMENT));
            let (Some(bang), Some(NodeOrToken::Node(tree))) = (after.next(), after.next()) else {
                return None;
            };
            if bang.kind() != SyntaxKind::BANG {
                return None;
            }
            tree
        }
    };
    let inside = tree
        .children_with_tokens()
        .filter(|e| {
            !matches!(
                e.kind(),
                SyntaxKind::WHITESPACE | SyntaxKind::COMMENT | SyntaxKind::L_PAREN
            )
        })
        .collect::<Vec<_>>();
    let (path, prefix) = match inside.as_slice() {
        [first, ..] if first.kind() == SyntaxKind::STRING => (literal(first)?, false),
        [first, _, NodeOrToken::Node(inner), ..] if first.to_string() == "concat" => (
            inner.descendants_with_tokens().find_map(|e| literal(&e))?,
            true,
        ),
        _ => return None,
    };
    Some(Embed {
        offset: u32::from(name.text_range().start()) as usize,
        path,
        prefix,
    })
}

/// The entries of a macro invocation.
fn read_macro(call: &ast::MacroCall, outline: &mut Outline) {
    let Some(tree) = call.token_tree() else {
        return;
    };
    let children = tree
        .syntax()
        .children_with_tokens()
        .filter(|child| {
            !matches!(
                child.kind(),
                SyntaxKind::WHITESPACE
                    | SyntaxKind::COMMENT
                    | SyntaxKind::L_PAREN
                    | SyntaxKind::L_CURLY
                    | SyntaxKind::L_BRACK
            )
        })
        .collect::<Vec<_>>();
    // Entries split at the separators of the macro's own token tree: each
    // that starts with an identifier, and where it starts.
    let mut entry: Option<(Option<String>, usize)> = None;
    for child in &children {
        let from = u32::from(child.text_range().start()) as usize;
        if matches!(
            child.kind(),
            SyntaxKind::COMMA
                | SyntaxKind::SEMICOLON
                | SyntaxKind::R_PAREN
                | SyntaxKind::R_CURLY
                | SyntaxKind::R_BRACK
        ) {
            if let Some((Some(name), start)) = entry.take() {
                outline.entries.push((name, (start, from)));
            }
            continue;
        }
        entry.get_or_insert_with(|| {
            (
                (child.kind() == SyntaxKind::IDENT).then(|| child.to_string()),
                from,
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_test_file_names_its_literals_tests_entries_embeds_and_modules() {
        let source = r#"
mod common;
mod inline { fn helper() -> &'static str { "inline.txt" } }
#[cfg(test)]
mod checks {}
const PAGE: &str = include_str!("../assets/page.html");
macro_rules! gates { ($($name:ident => $script:literal),*) => {} }
macro_rules! files { ($($name:literal),*) => { &[$(include_bytes!(concat!("../runtime/", $name))),*] } }
gates! {
    first => "scripts/first.mjs",
    second => "scripts/second.mjs"
}
fn shared() -> &'static str { "tests/fixtures/shared" }
#[test]
fn reads_a_fixture() {
    let path = format!("tests/fixtures/{}", "one");
    let _ = include_bytes!(concat!("../assets/", "two"));
    let _ = (path, vec![1, 2]);
}
#[tokio::test]
async fn awaits() {}
"#;
        let read = outline(source).expect("parses");
        let strings = read
            .strings
            .iter()
            .map(|(_, s)| s.as_str())
            .collect::<Vec<_>>();
        assert!(strings.contains(&"inline.txt"));
        assert!(strings.contains(&"tests/fixtures/{}"));
        assert_eq!(read.modules, ["common"]);
        assert_eq!(read.test_modules, ["checks"]);
        assert_eq!(
            read.embeds
                .iter()
                .map(|e| (e.path.as_str(), e.prefix))
                .collect::<Vec<_>>(),
            [
                ("../assets/page.html", false),
                ("../runtime/", true),
                ("../assets/", true)
            ]
        );
        let tests = read
            .functions
            .iter()
            .map(|f| (f.path.as_str(), f.test))
            .collect::<Vec<_>>();
        assert_eq!(
            tests,
            [
                ("inline::helper", false),
                ("shared", false),
                ("reads_a_fixture", true),
                ("awaits", true)
            ]
        );
        let first = read
            .entries
            .iter()
            .find(|(name, _)| name == "first")
            .expect("the first gate");
        assert_eq!(
            read.strings_in(first.1).collect::<Vec<_>>(),
            ["scripts/first.mjs"]
        );
        assert!(read.entries.iter().any(|(name, _)| name == "second"));
        // An embed whose path is computed names nothing, and neither does a
        // macro with nothing in it; separators side by side end no entry.
        let computed = outline("const X: &str = include_str!(env!(\"OUT\"));\nfn include() {}\nfn g() { include(); }\nfn f() { m!(a,, b); n!(); }\nmacro_rules! n { () => {} }\nmacro_rules! m { ($($t:tt)*) => {} }").unwrap();
        assert!(computed.embeds.is_empty());
        assert_eq!(
            computed
                .entries
                .iter()
                .map(|(name, _)| name.as_str())
                .collect::<Vec<_>>(),
            ["env", "a", "b"]
        );
        assert!(outline("fn (").is_none());
    }
}
