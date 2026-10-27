//! Ruby: statements by prism, tests by RSpec's scoped ids, Minitest and
//! test-unit method names, and Cucumber scenario lines.
use super::{Change, Located, Starts, line_of, line_starts};
use ruby_prism::{Node, Visit};
use std::path::Path;

/// Every statement starting on each line, with its change: `if`, `unless` and
/// `elsif` inverted, `return x` and a method's or block's last expression
/// returning nil, a plain assignment's value becoming nil, anything else
/// skipped. Definitions and the calls that declare (`require`, `attr_reader`,
/// `private`, `include` ...) are not assessed.
pub(super) fn statement_starts(source: &str) -> Starts {
    let parsed = ruby_prism::parse(source.as_bytes());
    let mut collector = Collector {
        source,
        starts: line_starts(source),
        out: Starts::new(),
        last: Default::default(),
        lists: Default::default(),
    };
    collector.visit(&parsed.node());
    collector.out
}

struct Collector<'s> {
    source: &'s str,
    starts: Vec<usize>,
    out: Starts,
    /// Start offsets of the last expression of a method or block body.
    last: std::collections::BTreeSet<usize>,
    /// Statement lists that are expressions (`(a; b)`, `#{...}`).
    lists: std::collections::BTreeSet<usize>,
}

const DECLARING: &[&[u8]] = &[
    b"require",
    b"require_relative",
    b"autoload",
    b"attr_reader",
    b"attr_writer",
    b"attr_accessor",
    b"private",
    b"public",
    b"protected",
    b"module_function",
    b"private_constant",
    b"public_constant",
    b"include",
    b"extend",
    b"prepend",
    b"alias_method",
    b"private_class_method",
    b"public_class_method",
];

impl Collector<'_> {
    fn text(&self, start: usize, end: usize) -> String {
        self.source.get(start..end).unwrap_or("").to_owned()
    }

    fn push(&mut self, start: usize, end: usize, change: Option<Change>) {
        let text = self.text(start, end);
        self.out
            .entry(line_of(&self.starts, start))
            .or_default()
            .push((text, change));
    }

    fn change(&self, node: &Node<'_>) -> Option<Change> {
        let last = self.last.contains(&node.location().start_offset());
        // Returning nil or assigning nil, changed to nil, is the same code: a
        // question no test could answer yes to.
        let location = node.location();
        let statement = self.text(location.start_offset(), location.end_offset());
        let statement = statement.trim();
        if statement == "nil"
            || statement == "return nil"
            || statement.rsplit_once('=').is_some_and(|(head, value)| {
                value.trim() == "nil" && !head.ends_with(['=', '!', '<', '>'])
            })
        {
            return Some(Change::Skip);
        }
        if let Some(i) = node.as_if_node() {
            return Some(if i.if_keyword_loc().is_some() {
                Change::Invert
            } else if last {
                Change::ReturnUndefined
            } else {
                Change::Skip
            });
        }
        if node.as_unless_node().is_some() {
            return Some(Change::Invert);
        }
        if let Some(r) = node.as_return_node() {
            return Some(if r.arguments().is_some() {
                Change::ReturnUndefined
            } else {
                Change::Skip
            });
        }
        if node.as_local_variable_write_node().is_some()
            || node.as_instance_variable_write_node().is_some()
            || node.as_class_variable_write_node().is_some()
            || node.as_global_variable_write_node().is_some()
            || node.as_constant_write_node().is_some()
            || node.as_constant_path_write_node().is_some()
            || node.as_multi_write_node().is_some()
        {
            return Some(Change::ValueUndefined);
        }
        if node.as_def_node().is_some()
            || node.as_class_node().is_some()
            || node.as_module_node().is_some()
            || node.as_singleton_class_node().is_some()
            || node.as_alias_method_node().is_some()
            || node.as_alias_global_variable_node().is_some()
            || node.as_undef_node().is_some()
        {
            return None;
        }
        if let Some(call) = node.as_call_node()
            && call.receiver().is_none()
            && DECLARING.contains(&call.name().as_slice())
        {
            return None;
        }
        Some(if last {
            Change::ReturnUndefined
        } else {
            Change::Skip
        })
    }

    fn mark_last(&mut self, body: Option<Node<'_>>) {
        if let Some(last) = body
            .and_then(|b| b.as_statements_node())
            .and_then(|statements| statements.body().iter().last())
        {
            self.last.insert(last.location().start_offset());
        }
    }
}

impl<'pr> Visit<'pr> for Collector<'_> {
    fn visit_statements_node(&mut self, node: &ruby_prism::StatementsNode<'pr>) {
        if !self.lists.contains(&node.location().start_offset()) {
            for statement in node.body().iter() {
                let change = self.change(&statement);
                let l = statement.location();
                self.push(l.start_offset(), l.end_offset(), change);
            }
        }
        ruby_prism::visit_statements_node(self, node);
    }

    fn visit_parentheses_node(&mut self, node: &ruby_prism::ParenthesesNode<'pr>) {
        if let Some(body) = node.body() {
            self.lists.insert(body.location().start_offset());
        }
        ruby_prism::visit_parentheses_node(self, node);
    }

    fn visit_embedded_statements_node(&mut self, node: &ruby_prism::EmbeddedStatementsNode<'pr>) {
        if let Some(statements) = node.statements() {
            self.lists.insert(statements.location().start_offset());
        }
        ruby_prism::visit_embedded_statements_node(self, node);
    }

    fn visit_def_node(&mut self, node: &ruby_prism::DefNode<'pr>) {
        self.mark_last(node.body());
        ruby_prism::visit_def_node(self, node);
    }

    fn visit_block_node(&mut self, node: &ruby_prism::BlockNode<'pr>) {
        self.mark_last(node.body());
        ruby_prism::visit_block_node(self, node);
    }

    fn visit_lambda_node(&mut self, node: &ruby_prism::LambdaNode<'pr>) {
        self.mark_last(node.body());
        ruby_prism::visit_lambda_node(self, node);
    }

    fn visit_if_node(&mut self, node: &ruby_prism::IfNode<'pr>) {
        // An `elsif` is an `if` of its own; coverage records its predicate.
        if node.if_keyword_loc().is_some() {
            let mut next = node.subsequent();
            while let Some(n) = next {
                let Some(elsif) = n.as_if_node() else { break };
                let p = elsif.predicate().location();
                self.push(p.start_offset(), p.end_offset(), Some(Change::Invert));
                next = elsif.subsequent();
            }
        }
        ruby_prism::visit_if_node(self, node);
    }
}

/// `if c`, `elsif c`, `unless c`, `x if c` -> `c`
pub(super) fn condition(line: &str) -> String {
    let t = line.trim();
    for keyword in ["elsif ", "if ", "unless "] {
        if let Some(rest) = t.strip_prefix(keyword) {
            return rest.trim_end_matches(" then").trim().to_owned();
        }
    }
    for keyword in [" if ", " unless "] {
        if let Some((_, rest)) = t.rsplit_once(keyword) {
            return rest.trim().to_owned();
        }
    }
    t.to_owned()
}

const GROUPS: &[&[u8]] = &[
    b"describe",
    b"context",
    b"feature",
    b"example_group",
    b"xdescribe",
    b"xcontext",
    b"fdescribe",
    b"fcontext",
    b"it_behaves_like",
    b"it_should_behave_like",
];
const EXAMPLES: &[&[u8]] = &[
    b"it",
    b"specify",
    b"example",
    b"scenario",
    b"its",
    b"fit",
    b"fspecify",
    b"fexample",
    b"xit",
    b"xspecify",
    b"xexample",
    b"pending",
    b"skip",
];

/// One RSpec runnable in source order: a group with its children, or an
/// example. `uncounted` marks one whose number of runtime runnables the source
/// does not say (inside a loop, or an `include_examples`).
struct Runnable {
    lines: (usize, usize),
    children: Vec<Runnable>,
    group: bool,
    uncounted: bool,
}

struct Specs<'s> {
    starts: &'s [usize],
    stack: Vec<Vec<Runnable>>,
    loops: usize,
}

impl Specs<'_> {
    /// A runnable of the group being read. The file's own level is pushed
    /// first and only `rspec` pops it, so there is always one.
    fn push(&mut self, runnable: Runnable) {
        self.stack
            .last_mut()
            .expect("the file's level")
            .push(runnable);
    }
}

impl<'pr> Visit<'pr> for Specs<'_> {
    fn visit_call_node(&mut self, node: &ruby_prism::CallNode<'pr>) {
        let name = node.name().as_slice();
        let rspec_receiver = node
            .receiver()
            .and_then(|r| r.as_constant_read_node())
            .is_some_and(|c| c.name().as_slice() == b"RSpec");
        let bare = node.receiver().is_none() || rspec_receiver;
        let l = node.location();
        let lines = (
            line_of(self.starts, l.start_offset()),
            line_of(self.starts, l.end_offset().saturating_sub(1)),
        );
        if bare && GROUPS.contains(&name) {
            let shared = name.starts_with(b"it_");
            self.stack.push(Vec::new());
            if !shared && let Some(block) = node.block() {
                self.visit(&block);
            }
            let children = self.stack.pop().unwrap_or_default();
            self.push(Runnable {
                lines,
                children,
                group: true,
                uncounted: self.loops > 0,
            });
            return;
        }
        if bare && EXAMPLES.contains(&name) && (node.block().is_some() || name != b"skip") {
            self.push(Runnable {
                lines,
                children: Vec::new(),
                group: false,
                uncounted: self.loops > 0,
            });
            return;
        }
        if bare && (name == b"include_examples" || name == b"include_context") {
            self.push(Runnable {
                lines,
                children: Vec::new(),
                group: false,
                uncounted: true,
            });
            return;
        }
        if bare
            && (name == b"shared_examples"
                || name == b"shared_context"
                || name == b"shared_examples_for")
        {
            return;
        }
        if let Some(receiver) = node.receiver() {
            self.visit(&receiver);
        }
        if let Some(arguments) = node.arguments() {
            self.visit_arguments_node(&arguments);
        }
        if let Some(block) = node.block() {
            self.loops += 1;
            self.visit(&block);
            self.loops -= 1;
        }
    }
}

/// An RSpec example by its scoped id (`[1:2:3]`: the third runnable of the
/// second of the first group), or the deepest group the source can name when
/// a loop or shared examples make the numbering a runtime matter.
fn rspec(source: &str, ids: &[usize]) -> Option<Located> {
    let starts = line_starts(source);
    let parsed = ruby_prism::parse(source.as_bytes());
    let mut specs = Specs {
        starts: &starts,
        stack: vec![Vec::new()],
        loops: 0,
    };
    specs.visit(&parsed.node());
    let mut level = specs.stack.pop()?;
    let mut context = Vec::new();
    let mut found: Option<(usize, usize)> = None;
    for &id in ids {
        let exact = !level.iter().take(id.saturating_sub(1)).any(|r| r.uncounted);
        let Some(pick) = exact
            .then(|| id.checked_sub(1))
            .flatten()
            .filter(|&i| i < level.len())
        else {
            break;
        };
        let r = level.swap_remove(pick);
        if let Some(outer) = found {
            context.push(outer.0);
        }
        found = Some(r.lines);
        if !r.group {
            break;
        }
        level = r.children;
    }
    let (start, end) = found?;
    Some(Located {
        context: context.into_iter().map(|l| l - 1).collect(),
        start: start - 1,
        end: end - 1,
    })
}

/// The test's code for each Ruby runner.
pub(super) fn locate(source: &str, lines: &[&str], runner: &str, name: &str) -> Option<Located> {
    match runner {
        "rspec" => {
            let ids = name
                .rsplit_once('[')
                .map(|(_, r)| r.trim_end_matches(']'))?
                .split(':')
                .filter_map(|n| n.parse().ok())
                .collect::<Vec<usize>>();
            rspec(source, &ids)
        }
        "cucumber" => {
            let line: usize = name.rsplit_once(':')?.1.parse().ok()?;
            let start = line.checked_sub(1)?;
            let indent = super::indent(lines.get(start)?);
            let end = (start + 1..lines.len())
                .find(|&i| {
                    let t = lines[i].trim_start();
                    super::indent(lines[i]) <= indent
                        && (t.starts_with("Scenario")
                            || t.starts_with("Example")
                            || t.starts_with('@'))
                })
                .map_or(lines.len() - 1, |i| i - 1);
            Some(Located {
                context: Vec::new(),
                start,
                end,
            })
        }
        _ => {
            // Minitest and test-unit: `Class#method`. Spec-style tests are
            // methods named after their description.
            let method = name.rsplit_once('#').map_or(name, |(_, m)| m);
            let described = method
                .strip_prefix("test_")
                // minitest/spec numbers each `it`: `test_0001_adds`. A word
                // of four letters is not a number (`test_adds_numbers`).
                .and_then(|r| {
                    r.get(5..).filter(|_| {
                        r.as_bytes().get(4) == Some(&b'_')
                            && r.as_bytes()[..4].iter().all(u8::is_ascii_digit)
                    })
                })
                .or_else(|| method.strip_prefix("test: "))
                .map(str::to_owned)
                .or_else(|| method.strip_prefix("test_").map(|r| r.replace('_', " ")));
            // Test classes in one file often share method names: look in the
            // class the name gives (`Shop::CartTest#test_total`) first.
            let class = name
                .rsplit_once('#')
                .map(|(class, _)| class.rsplit("::").next().unwrap_or(class));
            let (from, to) = class
                .and_then(|class| {
                    let at = lines.iter().position(|l| {
                        l.trim_start()
                            .strip_prefix("class ")
                            .and_then(|rest| rest.strip_prefix(class))
                            .is_some_and(|rest| {
                                rest.is_empty() || rest.starts_with([' ', '<', ';'])
                            })
                    })?;
                    Some((at, super::keyword_end(lines, at) + 1))
                })
                .unwrap_or((0, lines.len()));
            let defines = |i: &usize| {
                let t = lines[*i].trim_start();
                t.strip_prefix("def ").is_some_and(|r| {
                    r.strip_prefix(method)
                        .is_some_and(|r| r.is_empty() || r.starts_with(['(', ' ', ';']))
                })
            };
            let describes = |i: &usize| {
                described.as_deref().is_some_and(|d| {
                    let t = lines[*i].trim_start();
                    ["it ", "test ", "it(", "test("]
                        .iter()
                        .any(|k| t.starts_with(k))
                        && (t.contains(&format!("\"{d}\"")) || t.contains(&format!("'{d}'")))
                })
            };
            let to = to.min(lines.len());
            let start = (from..to)
                .find(defines)
                .or_else(|| (0..lines.len()).find(defines))
                .or_else(|| (from..to).find(describes))
                .or_else(|| (0..lines.len()).find(describes))?;
            let class = class_line(lines, start);
            Some(Located {
                context: class.into_iter().collect(),
                start,
                end: super::keyword_end(lines, start),
            })
        }
    }
}

fn class_line(lines: &[&str], start: usize) -> Option<usize> {
    let indent = super::indent(lines[start]);
    (0..start).rev().find(|&i| {
        let t = lines[i].trim_start();
        (t.starts_with("class ") || t.starts_with("describe ")) && super::indent(lines[i]) < indent
    })
}

/// A Ruby test's helpers: the files it requires that exist in the project
/// (`require_relative` from its own directory; `require` from `spec/`,
/// `test/`, `features/support/` or the root).
pub(super) fn helpers(root: &Path, from: &str, text: &str) -> Vec<String> {
    let here = Path::new(from).parent().unwrap_or(Path::new(""));
    // What `.rspec` or the Rakefile loads before every test file.
    let mut out = [
        "spec/spec_helper.rb",
        "test/test_helper.rb",
        "features/support/env.rb",
    ]
    .into_iter()
    .filter(|h| from.starts_with(h.split('/').next().unwrap_or("")) && root.join(h).is_file())
    .map(str::to_owned)
    .collect::<Vec<_>>();
    for line in text.split('\n') {
        let t = line.trim_start();
        let (relative, rest) = if let Some(r) = t.strip_prefix("require_relative") {
            (true, r)
        } else if let Some(r) = t.strip_prefix("require") {
            (false, r)
        } else {
            continue;
        };
        let rest = rest.trim_start().trim_start_matches('(');
        let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            continue;
        };
        let Some(name) = rest[1..].split(q).next() else {
            continue;
        };
        let name = name.strip_suffix(".rb").unwrap_or(name);
        let bases: Vec<std::path::PathBuf> = if relative {
            vec![here.to_path_buf()]
        } else {
            ["spec", "test", "features/support", ""]
                .iter()
                .map(Into::into)
                .collect()
        };
        for base in bases {
            let path = super::normalize(&base.join(format!("{name}.rb")));
            if root.join(&path).is_file() {
                out.push(path);
                break;
            }
        }
    }
    out
}

/// The support files a test names by constant: `Fixtures::Pluralize.cases`
/// reads `spec/support/fixtures/pluralize.rb`. Data-driven specs keep their
/// cases there, and a test's verdict depends on which cases it checks.
pub(super) fn support_files(root: &Path, text: &str) -> Vec<String> {
    let mut names = std::collections::BTreeSet::new();
    for token in text.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == ':')) {
        for segment in token.split("::") {
            if segment
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
            {
                names.insert(snake(segment));
            }
        }
    }
    let mut out = Vec::new();
    for dir in ["spec/support", "test/support", "features/support"] {
        let base = root.join(dir);
        if !base.is_dir() {
            continue;
        }
        // Every path the walk yields lies under `base`, so under `root`.
        let relatives = ignore::WalkBuilder::new(&base)
            .build()
            .filter_map(Result::ok)
            .filter_map(|entry| entry.path().strip_prefix(root).ok().map(Path::to_owned));
        for relative in relatives {
            if relative.extension().and_then(|x| x.to_str()) != Some("rb") {
                continue;
            }
            let stem = relative.file_stem().and_then(|s| s.to_str()).unwrap_or("");
            if names.contains(stem) {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    out.sort();
    out
}

/// `CamelizeLower` -> `camelize_lower`
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertion_coverage::{Language, change_at};

    #[test]
    fn nil_changed_to_nil_is_not_asked() {
        // The same code is a question no test could answer yes to.
        let source = "def f(x)\n  y = nil\n  return nil if x\n  z = x\n  nil\nend\n";
        let starts = statement_starts(source);
        let rb = Language::Ruby;
        assert_eq!(change_at(&starts, 2, "y = nil", rb), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 4, "z = x", rb),
            Some(Change::ValueUndefined)
        );
        assert_eq!(change_at(&starts, 5, "nil", rb), Some(Change::Skip));
    }

    #[test]
    fn changes_follow_the_audit() {
        let source = "require \"x\"\n\nclass A\n  attr_reader :a\n\n  def f(x)\n    y = x + 1\n    return y if y > 2\n    if x\n      log(x)\n    elsif y\n      nil\n    end\n    y * 2\n  end\nend\n";
        let starts = statement_starts(source);
        let rb = Language::Ruby;
        assert_eq!(change_at(&starts, 1, "require \"x\"", rb), None);
        assert_eq!(change_at(&starts, 4, "attr_reader :a", rb), None);
        assert_eq!(change_at(&starts, 6, "def f(x)", rb), None);
        assert_eq!(
            change_at(&starts, 7, "y = x + 1", rb),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 8, "return y if y > 2", rb),
            Some(Change::Invert)
        );
        assert_eq!(change_at(&starts, 10, "log(x)", rb), Some(Change::Skip));
        assert_eq!(change_at(&starts, 11, "y", rb), Some(Change::Invert));
        assert_eq!(
            change_at(&starts, 14, "y * 2", rb),
            Some(Change::ReturnUndefined)
        );
        assert_eq!(condition("return y if y > 2"), "y > 2");
        assert_eq!(condition("elsif a && b"), "a && b");
    }

    /// Shared examples are not counted; `skip` is an example only with a
    /// block; `it_behaves_like` is a group whose body is elsewhere; and once
    /// `include_examples` stands before an example, its id no longer says
    /// which it is and the group holding it is what is known.
    /// An `it` with no block is a pending example RSpec numbers, a group
    /// with no block holds nothing, and `shared_examples_for` defines
    /// rather than runs.
    #[test]
    fn pending_examples_empty_groups_and_shared_definitions_are_numbered_as_rspec_does() {
        let spec = "RSpec.describe A do\n  shared_examples_for \"s\" do\n    it \"in shared\" do\n    end\n  end\n  it \"pending\"\n  describe \"empty\"\n  it \"last\" do\n  end\nend\n";
        let lines = spec.split('\n').collect::<Vec<_>>();
        let at = |id: &str| {
            let found = locate(spec, &lines, "rspec", &format!("spec/a_spec.rb[{id}]")).unwrap();
            (found.start + 1, found.end + 1)
        };
        assert_eq!(at("1:1"), (6, 6));
        assert_eq!(at("1:2"), (7, 7));
        assert_eq!(at("1:3"), (8, 9));
    }

    /// A lambda's last statement is its value, as a method's is.
    #[test]
    fn a_lambda_s_last_statement_is_its_value() {
        let source = "handler = ->(x) do\n  y = x + 1\n  y * 2\nend\n";
        let starts = statement_starts(source);
        let rb = Language::Ruby;
        assert_eq!(
            change_at(&starts, 2, "y = x + 1", rb),
            Some(Change::ValueUndefined)
        );
        assert_eq!(
            change_at(&starts, 3, "y * 2", rb),
            Some(Change::ReturnUndefined)
        );
    }

    /// A ternary that is not a method's value is skipped; empty parentheses
    /// and an empty interpolation hold no statement of their own.
    #[test]
    fn a_ternary_mid_method_and_empty_lists_are_read() {
        let source = "def f(x)\n  x ? 1 : 2\n  ()\n  \"#{}\"\n  3\nend\n";
        let starts = statement_starts(source);
        let rb = Language::Ruby;
        assert_eq!(change_at(&starts, 2, "x ? 1 : 2", rb), Some(Change::Skip));
        assert_eq!(change_at(&starts, 3, "()", rb), Some(Change::Skip));
        assert_eq!(change_at(&starts, 4, "\"#{}\"", rb), Some(Change::Skip));
        assert_eq!(
            change_at(&starts, 5, "3", rb),
            Some(Change::ReturnUndefined)
        );
    }

    #[test]
    fn rspec_ids_count_only_what_rspec_numbers() {
        let spec = "RSpec.shared_examples \"s\" do\n  it \"in shared\" do\n  end\nend\ndescribe A do\n  skip \"no block\"\n  skip \"with block\" do\n  end\n  it_behaves_like \"s\"\n  include_examples \"s\"\n  it \"after include\" do\n  end\nend\n";
        let lines = spec.split('\n').collect::<Vec<_>>();
        let at = |id: &str| {
            locate(spec, &lines, "rspec", &format!("spec/a_spec.rb[{id}]"))
                .map(|found| (found.start, found.end))
        };
        assert_eq!(at("1:1"), Some((6, 7)));
        assert_eq!(at("1:2"), Some((8, 8)));
        // Not exact any more, or past the end: the group that holds it.
        assert_eq!(at("1:4"), Some((4, 12)));
        assert_eq!(at("1:9"), Some((4, 12)));
        assert_eq!(at("2"), None);
        assert!(locate(spec, &lines, "rspec", "no brackets").is_none());
    }

    #[test]
    fn tests_are_found_by_runner() {
        let spec = "RSpec.describe A do\n  it \"one\" do\n    expect(1).to eq(1)\n  end\n\n  context \"more\" do\n    %w[a b].each do |x|\n      it \"loops #{x}\" do\n      end\n    end\n    it \"two\" do\n      expect(2).to eq(2)\n    end\n  end\nend\n";
        let lines = spec.split('\n').collect::<Vec<_>>();
        let one = locate(spec, &lines, "rspec", "spec/a_spec.rb[1:1]").unwrap();
        assert_eq!((one.start, one.end), (1, 3));
        // Behind a loop the numbering is a runtime matter: the group is shown.
        let two = locate(spec, &lines, "rspec", "spec/a_spec.rb[1:2:3]").unwrap();
        assert_eq!((two.start, two.end), (5, 13));
        let mini = "class ATest < Minitest::Test\n  def test_adds\n    assert_equal 2, 1 + 1\n  end\nend\n";
        let lines = mini.split('\n').collect::<Vec<_>>();
        let found = locate(mini, &lines, "minitest", "ATest#test_adds").unwrap();
        assert_eq!(
            (found.context.clone(), found.start, found.end),
            (vec![0], 1, 3)
        );
    }

    /// RSpec numbers runnables as it runs them: a shared group's definition
    /// is none, `it_behaves_like` is one group, and `include_examples` adds
    /// examples the source does not count, so a later number is shown as its
    /// group.
    /// Every kind of write becomes undefined, a comparison with nil is no
    /// nil assigned, and what declares rather than runs is not asked.
    #[test]
    fn ruby_writes_comparisons_and_declarations_are_told_apart() {
        let source = "module M\n  def g(x)\n    @a = 1\n    @@b = 2\n    $c = 3\n    a, b = 1, 2\n    return\n    x == nil\n    unless x then 5 end\n    x ? 1 : 2\n  end\n  alias h g\n  class << self; end\n  undef h\n  CONST = 1\n  M::X = 2\nend\n";
        let starts = statement_starts(source);
        let rb = Language::Ruby;
        for (line, text, change) in [
            (1, "module M", None),
            (3, "@a = 1", Some(Change::ValueUndefined)),
            (4, "@@b = 2", Some(Change::ValueUndefined)),
            (5, "$c = 3", Some(Change::ValueUndefined)),
            (6, "a, b = 1, 2", Some(Change::ValueUndefined)),
            (7, "return", Some(Change::Skip)),
            (8, "x == nil", Some(Change::Skip)),
            (9, "unless x then 5 end", Some(Change::Invert)),
            (10, "x ? 1 : 2", Some(Change::ReturnUndefined)),
            (12, "alias h g", None),
            (13, "class << self; end", None),
            (14, "undef h", None),
            (15, "CONST = 1", Some(Change::ValueUndefined)),
            (16, "M::X = 2", Some(Change::ValueUndefined)),
        ] {
            assert_eq!(
                change_at(&starts, line, text, rb),
                change,
                "line {line}: {text}"
            );
        }
    }

    /// An example made in a loop is numbered where it is written once; how
    /// many the loop made is unknown, so an id after it names the group.
    /// `skip` with no block runs nothing.
    #[test]
    fn examples_made_in_a_loop_leave_their_group_named() {
        let spec = "RSpec.describe A do\n  skip \"later\"\n  it \"first\" do\n  end\n  [1, 2].each do |n|\n    it \"each #{n}\" do\n    end\n  end\n  shared_context \"ctx\" do\n  end\n  include_context \"ctx\"\n  it \"after\" do\n  end\nend\n";
        let lines = spec.split('\n').collect::<Vec<_>>();
        let at = |id: &str| {
            let found = locate(spec, &lines, "rspec", &format!("spec/a_spec.rb[{id}]")).unwrap();
            (found.start + 1, found.end + 1)
        };
        assert_eq!(at("1:1"), (3, 4), "skip with no block is no example");
        assert_eq!(at("1:2"), (6, 7), "the first made in the loop");
        // How many the loop made is unknown: what follows names the group.
        assert_eq!(at("1:3"), (1, 14));
        assert_eq!(at("1:5"), (1, 14));
    }

    #[test]
    fn shared_examples_number_the_way_rspec_runs_them() {
        let spec = "RSpec.shared_examples \"a thing\" do\n  it \"works\" do\n  end\nend\n\nRSpec.describe A do\n  before { skip \"not here\" if ENV[\"CI\"] }\n  it_behaves_like \"a thing\"\n  it \"after\" do\n    expect(1).to eq(1)\n  end\n  include_examples \"a thing\"\n  it \"last\" do\n  end\nend\n";
        let lines = spec.split('\n').collect::<Vec<_>>();
        let at = |id: &str| {
            let found = locate(spec, &lines, "rspec", &format!("spec/a_spec.rb[{id}]")).unwrap();
            (found.start + 1, found.end + 1)
        };
        assert_eq!(at("1"), (6, 15), "the describe, not the shared definition");
        assert_eq!(at("1:1"), (8, 8), "it_behaves_like is one group");
        assert_eq!(at("1:2"), (9, 11));
        assert_eq!(at("1:3"), (12, 12));
        assert_eq!(at("1:4"), (6, 15), "after include_examples: the group");
    }

    #[test]
    fn scenarios_and_described_tests_are_found_by_their_names() {
        let feature = "Feature: cart\n\n  Scenario: adds\n    Given a cart\n    Then it adds\n\n  @slow\n  Scenario: removes\n    Given a cart\n";
        let lines = feature.split('\n').collect::<Vec<_>>();
        let adds = locate(feature, &lines, "cucumber", "features/cart.feature:3").unwrap();
        assert_eq!(
            (adds.start, adds.end),
            (2, 5),
            "up to the next scenario's tag"
        );
        let last = locate(feature, &lines, "cucumber", "features/cart.feature:8").unwrap();
        assert_eq!((last.start, last.end), (7, lines.len() - 1));
        // Rails' `test "adds numbers"`, minitest/spec's numbered `it`, and a
        // method named for a word the description starts with.
        let tests = "class CartTest < ActiveSupport::TestCase\n  test \"adds numbers\" do\n    assert true\n  end\nend\ndescribe Cart do\n  it 'removes items' do\n    assert true\n  end\nend\nclass Plain < Minitest::Test\n  def test_adds_more; assert true; end\n  def test_adds\n    assert true\n  end\nend\n";
        let lines = tests.split('\n').collect::<Vec<_>>();
        let line = |name: &str| {
            let found = locate(tests, &lines, "minitest", name).unwrap();
            (found.context, found.start + 1)
        };
        assert_eq!(line("CartTest#test_adds_numbers"), (vec![0], 2));
        assert_eq!(line("Cart#test_0001_removes items"), (vec![5], 7));
        assert_eq!(line("Plain#test_adds"), (vec![10], 13));
        assert!(locate(tests, &lines, "minitest", "Plain#test_missing").is_none());
        // Two classes, one method name: each test in its own class.
        let two = "class CartTest < Minitest::Test\n  def test_total\n    assert_equal 0, cart\n  end\nend\n\nclass OrderTest < Minitest::Test\n  def test_total\n    assert_equal 2, order\n  end\nend\n";
        let lines = two.split('\n').collect::<Vec<_>>();
        let start = |name: &str| locate(two, &lines, "minitest", name).unwrap().start + 1;
        assert_eq!(start("OrderTest#test_total"), 8);
        assert_eq!(start("Shop::CartTest#test_total"), 2);
    }

    /// The remaining name shapes: a scenario ended by an `Example`, a class
    /// line with nothing after its name, a method with arguments, a
    /// test-unit `test: ` name; and the files a test requires.
    #[test]
    fn remaining_name_shapes_and_required_helpers_are_found() {
        let feature = "Feature: f\n  Scenario: one\n    Given x\n  Example: two\n    Given y\n";
        let lines = feature.split('\n').collect::<Vec<_>>();
        let one = locate(feature, &lines, "cucumber", "features/f.feature:2").unwrap();
        assert_eq!((one.start, one.end), (1, 2));
        let feature = "Feature: f\n  Scenario: one\n    Given x\n  Scenario: two\n";
        let lines = feature.split('\n').collect::<Vec<_>>();
        let one = locate(feature, &lines, "cucumber", "features/f.feature:2").unwrap();
        assert_eq!((one.start, one.end), (1, 2), "up to the next scenario");
        assert!(locate(feature, &lines, "cucumber", "features/f.feature:x").is_none());
        let tests = "class Bare\n  def test_args(a)\n    assert a\n  end\n  test \"adds two\" do\n    assert true\n  end\nend\n";
        let lines = tests.split('\n').collect::<Vec<_>>();
        let found = locate(tests, &lines, "minitest", "Bare#test_args").unwrap();
        assert_eq!((found.context, found.start), (vec![0], 1));
        let found = locate(tests, &lines, "test-unit", "Bare#test: adds two").unwrap();
        assert_eq!(found.start, 4);
        let root = std::env::temp_dir().join(format!(
            "supercov-ruby-helpers-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("spec/support")).unwrap();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        for file in [
            "spec/spec_helper.rb",
            "spec/support/money.rb",
            "spec/support/unrequired.rb",
            "spec/local.rb",
            "lib/shop.rb",
        ] {
            std::fs::write(root.join(file), "\n").unwrap();
        }
        let text = "require 'support/money'\nrequire_relative(\"local\")\nrequire_relative \"missing\"\nrequire lib_path\nrequire 'lib/shop.rb'\nputs 1\n";
        assert_eq!(
            helpers(&root, "spec/cart_spec.rb", text),
            [
                "spec/spec_helper.rb",
                "spec/support/money.rb",
                "spec/local.rb",
                "lib/shop.rb"
            ]
        );
        // Only the support file a constant names.
        assert_eq!(
            support_files(&root, "expect(Money.parse(1)).to eq 1\n"),
            ["spec/support/money.rb"]
        );
    }

    /// A class nested no shallower than the test is not the test's class.
    #[test]
    fn a_test_belongs_to_the_class_around_it_not_a_sibling_before_it() {
        let source =
            "class Outer\n  class Helper\n    def x; end\n  end\n  def test_y\n  end\nend\n";
        let lines = source.split('\n').collect::<Vec<_>>();
        assert_eq!(class_line(&lines, 4), Some(0));
        assert_eq!(class_line(&lines, 2), Some(1));
    }
}
