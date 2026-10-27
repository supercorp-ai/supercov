//! Go: statements by tree-sitter, tests by their function name.
use super::{Change, Located, Starts, line_of, line_starts};
use std::path::Path;
use tree_sitter::{Node, Parser, Tree};

/// Every statement starting on each line, with its change: `if` inverted,
/// `return x` returning zero values, an assignment or declaration with a value
/// leaving the zero value, anything else skipped. Declarations of functions,
/// types, constants and imports, and `var` without a value, are not assessed.
pub(super) fn statement_starts(source: &str) -> Starts {
    let mut out = Starts::new();
    let Some(tree) = parse(source) else {
        return out;
    };
    let starts = line_starts(source);
    walk(tree.root_node(), source, &starts, &mut out);
    out
}

/// The tree of a Go source, a broken one included.
fn parse(source: &str) -> Option<Tree> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_go::LANGUAGE.into()).ok()?;
    parser.parse(source, None)
}

fn walk(node: Node, source: &str, starts: &[usize], out: &mut Starts) {
    if let Some(change) = change(node) {
        let text = source
            .get(node.start_byte()..node.end_byte())
            .unwrap_or("")
            .to_owned();
        out.entry(line_of(starts, node.start_byte()))
            .or_default()
            .push((text, change));
    }
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, source, starts, out);
    }
}

/// `Some(change)` for a statement (`Some(None)`: a statement not assessed),
/// `None` for anything that is not one.
fn change(node: Node) -> Option<Option<Change>> {
    let parent = node.parent().map(|p| p.kind()).unwrap_or("");
    let in_body = matches!(parent, "block" | "statement_list" | "expression_case" | "type_case" | "default_case" | "communication_case" | "labeled_statement")
        // `else if`: the nested `if` is its own statement.
        || (node.kind() == "if_statement" && parent == "if_statement");
    if !in_body {
        return None;
    }
    Some(match node.kind() {
        "if_statement" => Some(Change::Invert),
        "return_statement" => Some(if node.named_child_count() > 0 {
            Change::ReturnUndefined
        } else {
            Change::Skip
        }),
        "short_var_declaration" => Some(Change::ValueUndefined),
        "assignment_statement" => {
            let op = node.child_by_field_name("operator").map(|o| o.kind());
            Some(if op == Some("=") {
                Change::ValueUndefined
            } else {
                Change::Skip
            })
        }
        "var_declaration" => {
            let mut cursor = node.walk();
            let valued = node.named_children(&mut cursor).any(|spec| {
                spec.child_by_field_name("value").is_some() || {
                    let mut c = spec.walk();
                    spec.named_children(&mut c)
                        .any(|s| s.child_by_field_name("value").is_some())
                }
            });
            valued.then_some(Change::ValueUndefined)
        }
        "const_declaration"
        | "type_declaration"
        | "function_declaration"
        | "method_declaration"
        | "import_declaration"
        | "empty_statement" => None,
        "comment" => return None,
        k if k.ends_with("_statement") || k == "block" => Some(Change::Skip),
        _ => return None,
    })
}

/// The line a value change turns a statement into, with the zero values
/// written out: `return true` -> `return false`, `return uuid, nil` ->
/// `return UUID{}, nil` (from the function's result types), `x := &T{}` ->
/// `x := nil`. Nothing where a zero value cannot be written down.
pub(super) fn rewrite(source: &str, line: usize, change: Change) -> Option<String> {
    let tree = parse(source)?;
    let starts = line_starts(source);
    let text = |n: Node| source.get(n.byte_range()).unwrap_or("").to_owned();
    let node = find_statement(tree.root_node(), &starts, line, &|n| match change {
        Change::ReturnUndefined => n.kind() == "return_statement",
        Change::ValueUndefined => {
            matches!(n.kind(), "short_var_declaration" | "assignment_statement")
        }
        _ => false,
    })?;
    match change {
        Change::ReturnUndefined => {
            let results = result_types(node, source)?;
            let zeros = results.iter().map(|t| zero_of_type(t)).collect::<Vec<_>>();
            Some(format!("return {}", zeros.join(", ")))
        }
        Change::ValueUndefined => {
            let left = text(node.child_by_field_name("left")?);
            let right = node.child_by_field_name("right")?;
            let operator = if node.kind() == "short_var_declaration" {
                ":="
            } else {
                "="
            };
            let mut cursor = right.walk();
            let zeros = right
                .named_children(&mut cursor)
                .map(|value| zero_of_value(&text(value)))
                .collect::<Option<Vec<_>>>()?;
            Some(format!("{left} {operator} {}", zeros.join(", ")))
        }
        _ => None,
    }
}

/// The statement starting on `line` that `wanted` accepts.
fn find_statement<'a>(
    node: Node<'a>,
    starts: &[usize],
    line: usize,
    wanted: &dyn Fn(Node) -> bool,
) -> Option<Node<'a>> {
    if wanted(node) && line_of(starts, node.start_byte()) == line {
        return Some(node);
    }
    let mut cursor = node.walk();
    let children = node.children(&mut cursor).collect::<Vec<_>>();
    children.into_iter().find_map(|child| {
        let (first, last) = (
            line_of(starts, child.start_byte()),
            line_of(starts, child.end_byte()),
        );
        (first <= line && line <= last)
            .then(|| find_statement(child, starts, line, wanted))
            .flatten()
    })
}

/// The result types of the function a `return` leaves, one per value.
fn result_types(node: Node, source: &str) -> Option<Vec<String>> {
    let function = std::iter::successors(node.parent(), |n| n.parent()).find(|n| {
        matches!(
            n.kind(),
            "function_declaration" | "method_declaration" | "func_literal"
        )
    })?;
    let result = function.child_by_field_name("result")?;
    let text = |n: Node| source.get(n.byte_range()).unwrap_or("").to_owned();
    if result.kind() != "parameter_list" {
        return Some(vec![text(result)]);
    }
    let mut out = Vec::new();
    let mut cursor = result.walk();
    for declaration in result.named_children(&mut cursor) {
        let ty = text(declaration.child_by_field_name("type")?);
        let mut names = declaration.walk();
        let count = declaration
            .children_by_field_name("name", &mut names)
            .count()
            .max(1);
        out.extend(std::iter::repeat_n(ty, count));
    }
    Some(out)
}

const GO_NUMBERS: &[&str] = &[
    "int",
    "int8",
    "int16",
    "int32",
    "int64",
    "uint",
    "uint8",
    "uint16",
    "uint32",
    "uint64",
    "uintptr",
    "byte",
    "rune",
    "float32",
    "float64",
    "complex64",
    "complex128",
];

fn zero_of_type(ty: &str) -> String {
    let ty = ty.trim();
    if ty == "error"
        || ty == "any"
        || ["*", "[]", "map[", "chan ", "<-chan", "func", "interface"]
            .iter()
            .any(|p| ty.starts_with(p))
    {
        "nil".into()
    } else if ty == "bool" {
        "false".into()
    } else if ty == "string" {
        "\"\"".into()
    } else if GO_NUMBERS.contains(&ty) {
        "0".into()
    } else {
        format!("{ty}{{}}")
    }
}

/// The zero value of an expression's type, where its shape says the type.
fn zero_of_value(value: &str) -> Option<String> {
    let v = value.trim();
    if v == "nil"
        || ["&", "[]", "map[", "make(", "append(", "new(", "func("]
            .iter()
            .any(|p| v.starts_with(p))
        || v.starts_with("errors.New(")
        || v.starts_with("fmt.Errorf(")
    {
        return Some("nil".into());
    }
    if v == "true" || v == "false" {
        return Some("false".into());
    }
    if v.starts_with('"') || v.starts_with('`') {
        return Some("\"\"".into());
    }
    if v.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        return Some("0".into());
    }
    // `T{...}`: a composite literal of a named type.
    let (head, _) = v.split_once('{')?;
    (v.ends_with('}')
        && head
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.'))
    .then(|| format!("{head}{{}}"))
}

/// `if c {`, `} else if x := f(); c {` -> the condition as written.
pub(super) fn condition(line: &str) -> String {
    let t = line.trim();
    let t = t.strip_prefix('}').map(str::trim_start).unwrap_or(t);
    let t = t.strip_prefix("else").map(str::trim_start).unwrap_or(t);
    let t = t.strip_prefix("if ").unwrap_or(t);
    t.strip_suffix('{')
        .map(str::trim_end)
        .unwrap_or(t)
        .to_owned()
}

/// `TestParse`, `TestParse/with_urn` -> the function and its body.
pub(super) fn locate(lines: &[&str], name: &str) -> Option<Located> {
    let function = name.split('/').next().unwrap_or(name);
    let function = function.rsplit("::").next().unwrap_or(function);
    let start = lines.iter().position(|l| {
        l.strip_prefix("func ")
            .and_then(|r| r.strip_prefix(function))
            .is_some_and(|r| r.starts_with('(') || r.starts_with('['))
    })?;
    Some(Located {
        context: Vec::new(),
        start,
        end: super::brace_end(lines, start),
    })
}

/// A Go test's helpers: the package's other `_test.go` files that define a
/// function (not a test) this test's file calls.
pub(super) fn helpers(root: &Path, from: &str, text: &str) -> Vec<String> {
    let dir = Path::new(from).parent().unwrap_or(Path::new(""));
    let Ok(entries) = std::fs::read_dir(root.join(dir)) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.ends_with("_test.go"))
        .map(|n| dir.join(n).to_string_lossy().replace('\\', "/"))
        .filter(|p| p != from)
        .collect::<Vec<_>>();
    files.sort();
    files
        .into_iter()
        .filter(|file| {
            let Ok(body) = std::fs::read_to_string(root.join(file)) else {
                return false;
            };
            body.split('\n').any(|l| {
                let Some(rest) = l.strip_prefix("func ") else {
                    return false;
                };
                // A method's receiver comes first: `func (x T) name(`.
                let rest = match rest.strip_prefix('(') {
                    Some(r) => r.split_once(')').map_or("", |(_, r)| r.trim_start()),
                    None => rest,
                };
                let name = rest.split(['(', '[']).next().unwrap_or("");
                !name.is_empty()
                    && !["Test", "Benchmark", "Example", "Fuzz"]
                        .iter()
                        .any(|p| name.starts_with(p))
                    && text.contains(&format!("{name}("))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    /// hashicorp/go-version's `return true` and google/uuid's
    /// `return uuid, nil`: the zero values written out from the result types.
    #[test]
    fn a_value_change_is_written_as_the_line_it_becomes() {
        let source = "package v\n\nfunc Less(a int) bool {\n\treturn true\n}\n\nfunc New() (UUID, error) {\n\tuuid := &UUID{}\n\tvar err error\n\treturn *uuid, nil\n}\n\nfunc parts() (n int, s []string, err error) {\n\tsegments := append(s, \"x\")\n\tcount, name := 3, \"a\"\n\treturn len(segments), segments, nil\n}\n";
        assert_eq!(
            rewrite(source, 4, Change::ReturnUndefined).as_deref(),
            Some("return false")
        );
        assert_eq!(
            rewrite(source, 8, Change::ValueUndefined).as_deref(),
            Some("uuid := nil")
        );
        assert_eq!(
            rewrite(source, 10, Change::ReturnUndefined).as_deref(),
            Some("return UUID{}, nil")
        );
        assert_eq!(
            rewrite(source, 14, Change::ValueUndefined).as_deref(),
            Some("segments := nil")
        );
        assert_eq!(
            rewrite(source, 15, Change::ValueUndefined).as_deref(),
            Some("count, name := 0, \"\"")
        );
        assert_eq!(
            rewrite(source, 16, Change::ReturnUndefined).as_deref(),
            Some("return 0, nil, nil")
        );
    }

    use super::*;
    use crate::assertion_coverage::{Language, change_at};

    #[test]
    fn changes_follow_the_audit() {
        let source = "package a\n\nimport \"fmt\"\n\nfunc f(x int) (int, error) {\n\tvar y int\n\tz := x + 1\n\tif z > 2 {\n\t\treturn z, nil\n\t} else if z < 0 {\n\t\tz += 1\n\t}\n\tfmt.Println(z)\n\treturn 0, nil\n}\n";
        let starts = statement_starts(source);
        let go = Language::Go;
        assert_eq!(change_at(&starts, 3, "import \"fmt\"", go), None);
        assert_eq!(change_at(&starts, 6, "var y int", go), None);
        assert_eq!(
            change_at(&starts, 7, "z := x + 1", go),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 8, "if z > 2 {", go),
            Some(Change::Invert)
        );
        assert_eq!(
            change_at(&starts, 9, "return z, nil", go),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(
            change_at(&starts, 10, "if z < 0 {", go),
            Some(Change::Invert)
        );
        assert_eq!(change_at(&starts, 11, "z += 1", go), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 13, "fmt.Println(z)", go),
            Some(Change::Skip)
        );
        assert_eq!(condition("} else if x := f(); x > 0 {"), "x := f(); x > 0");
        let test =
            "package a\n\nfunc TestF(t *testing.T) {\n\tif f(1) != 2 {\n\t\tt.Fatal()\n\t}\n}\n";
        let lines = test.split('\n').collect::<Vec<_>>();
        let found = locate(&lines, "TestF/sub").unwrap();
        assert_eq!((found.start, found.end), (2, 6));
    }

    /// A value whose type only its shape says: a named type's composite
    /// literal is that type's empty one; a call says nothing.
    #[test]
    fn a_value_without_a_declared_type_is_zeroed_by_its_shape() {
        let source = "package a\n\nfunc f() {\n\tp := Point{X: 1, Y: 2}\n\tc := config.Options{Verbose: true}\n\tn := 42\n\tr := `raw`\n\tv := compute(n)\n\tm := Point{X: 1}.Scale(2)\n}\n";
        let at = |line| rewrite(source, line, Change::ValueUndefined);
        assert_eq!(at(4).as_deref(), Some("p := Point{}"));
        assert_eq!(at(5).as_deref(), Some("c := config.Options{}"));
        assert_eq!(at(6).as_deref(), Some("n := 0"));
        assert_eq!(at(7).as_deref(), Some("r := \"\""));
        assert_eq!(at(8), None);
        assert_eq!(at(9), None, "a method on a literal is not the literal");
    }

    /// A bare `return` and a block of its own are skipped, a `var` with a
    /// value or a grouped one changes its value, and a literal of a type
    /// with `_` in its name is zeroed as one.
    #[test]
    fn bare_returns_blocks_and_grouped_vars_are_read() {
        use crate::assertion_coverage::{Language, change_at};
        let source = "package a\n\nfunc f() {\n\tvar x = 1\n\tvar (\n\t\ty = 2\n\t)\n\t{\n\t\tg()\n\t}\n\tp := my_pkg.Point_3{X: 1}\n\treturn\n}\n";
        let starts = statement_starts(source);
        let go = Language::Go;
        assert_eq!(
            change_at(&starts, 4, "var x = 1", go),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 5, "var (", go),
            Some(Change::ValueUndefined)
        );
        assert_eq!(change_at(&starts, 8, "{", go), Some(Change::Skip));
        assert_eq!(change_at(&starts, 12, "return", go), Some(Change::Skip));
        assert_eq!(
            rewrite(source, 11, Change::ValueUndefined).as_deref(),
            Some("p := my_pkg.Point_3{}")
        );
        // An empty result list returns no value.
        assert_eq!(
            rewrite(
                "package a\n\nfunc f() () {\n\treturn\n}\n",
                4,
                Change::ReturnUndefined
            )
            .as_deref(),
            Some("return ")
        );
    }

    #[test]
    fn a_test_is_found_by_its_function_name_however_it_is_qualified() {
        let test = "package a\n\nfunc TestFormat(t *testing.T) {}\n\nfunc TestF[T any](t *testing.T) {\n}\n";
        let lines = test.split('\n').collect::<Vec<_>>();
        // `TestF` is not `TestFormat`: the name ends where its parameters or
        // type parameters begin.
        assert_eq!(locate(&lines, "a::TestF").unwrap().start, 4);
        assert_eq!(locate(&lines, "TestFormat/case").unwrap().start, 2);
        assert!(locate(&lines, "TestMissing").is_none());
    }

    /// Zero values by declared type and by a value's shape, conditions as
    /// written, and the package's helper files a test's file calls into.
    #[test]
    fn zeros_conditions_and_helpers_read_the_way_go_writes_them() {
        for (ty, zero) in [
            ("error", "nil"),
            ("any", "nil"),
            ("*T", "nil"),
            ("<-chan int", "nil"),
            ("interface{}", "nil"),
            ("bool", "false"),
            ("string", "\"\""),
            ("uint8", "0"),
            (" Point ", "Point{}"),
        ] {
            assert_eq!(zero_of_type(ty), zero, "{ty}");
        }
        for (value, zero) in [
            ("nil", Some("nil")),
            ("make([]int, 0)", Some("nil")),
            ("errors.New(\"x\")", Some("nil")),
            ("fmt.Errorf(\"x\")", Some("nil")),
            ("true", Some("false")),
            ("false", Some("false")),
            ("`raw`", Some("\"\"")),
            ("7", Some("0")),
            ("pkg.Point{X: 1}", Some("pkg.Point{}")),
            ("f(x){}", None),
            ("Point{X: 1}.Scale(2)", None),
            ("compute(n)", None),
        ] {
            assert_eq!(zero_of_value(value).as_deref(), zero, "{value}");
        }
        assert_eq!(condition("} else if x := f(); x > 1 {"), "x := f(); x > 1");
        assert_eq!(condition("if ok {"), "ok");
        assert_eq!(condition("ok"), "ok");
        assert!(locate(&["package a", "func TestX(t *testing.T) {", "}"], "TestY").is_none());
        let root = std::env::temp_dir().join(format!(
            "supercov-go-helpers-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("pkg")).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        for (file, body) in [
            (
                "pkg/a_test.go",
                "package pkg\nfunc TestA(t *testing.T) { build(); (&fixture{}).reset() }\n",
            ),
            ("pkg/build_test.go", "package pkg\nfunc build() {}\n"),
            (
                "pkg/fixture_test.go",
                "package pkg\nfunc (f *fixture) reset() {}\n",
            ),
            (
                "pkg/tests_test.go",
                "package pkg\nfunc TestOther(t *testing.T) {}\nfunc unused() {}\n",
            ),
            ("pkg/odd_test.go", "package pkg\nfunc (broken\n"),
            ("pkg/lib.go", "package pkg\nfunc build2() {}\n"),
        ] {
            std::fs::write(root.join(file), body).unwrap();
        }
        let text = std::fs::read_to_string(root.join("pkg/a_test.go")).unwrap();
        assert_eq!(
            helpers(&root, "pkg/a_test.go", &text),
            ["pkg/build_test.go", "pkg/fixture_test.go"]
        );
        assert!(helpers(&root, "missing/a_test.go", &text).is_empty());
    }

    /// A file that does not compile still has its statements read; an `if`
    /// outside any function is not one of them.
    #[test]
    fn an_if_outside_a_function_is_no_statement() {
        let source = "package a\n\nif ready {\n\trun()\n}\n\nfunc f() {\n\tif ready {\n\t}\n}\n";
        let starts = statement_starts(source);
        assert!(!starts.contains_key(&3), "{starts:?}");
        assert_eq!(
            starts.get(&8).map(Vec::as_slice),
            Some(&[("if ready {\n\t}".to_owned(), Some(Change::Invert))][..])
        );
    }

    #[test]
    fn a_line_between_declarations_has_nothing_to_rewrite() {
        let source = "package a\n\nfunc f() int {\n\treturn 1\n}\n";
        assert_eq!(rewrite(source, 2, Change::ReturnUndefined), None);
        assert_eq!(
            rewrite(source, 4, Change::ReturnUndefined).as_deref(),
            Some("return 0")
        );
    }
}
