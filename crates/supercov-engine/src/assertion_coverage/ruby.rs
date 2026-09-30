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
        if let Some(statements) = body.and_then(|b| b.as_statements_node())
            && let Some(last) = statements.body().iter().last()
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
            if let Some(level) = self.stack.last_mut() {
                level.push(Runnable {
                    lines,
                    children,
                    group: true,
                    uncounted: self.loops > 0,
                });
            }
            return;
        }
        if bare && EXAMPLES.contains(&name) && (node.block().is_some() || name != b"skip") {
            if let Some(level) = self.stack.last_mut() {
                level.push(Runnable {
                    lines,
                    children: Vec::new(),
                    group: false,
                    uncounted: self.loops > 0,
                });
            }
            return;
        }
        if bare && (name == b"include_examples" || name == b"include_context") {
            if let Some(level) = self.stack.last_mut() {
                level.push(Runnable {
                    lines,
                    children: Vec::new(),
                    group: false,
                    uncounted: true,
                });
            }
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
                .and_then(|r| r.get(5..).filter(|_| r.as_bytes().get(4) == Some(&b'_')))
                .or_else(|| method.strip_prefix("test: "))
                .map(str::to_owned)
                .or_else(|| method.strip_prefix("test_").map(|r| r.replace('_', " ")));
            let start = lines
                .iter()
                .position(|l| {
                    let t = l.trim_start();
                    t.strip_prefix("def ").is_some_and(|r| {
                        r.strip_prefix(method)
                            .is_some_and(|r| r.is_empty() || r.starts_with(['(', ' ', ';']))
                    })
                })
                .or_else(|| {
                    let d = described.as_deref()?;
                    lines.iter().position(|l| {
                        let t = l.trim_start();
                        ["it ", "test ", "it(", "test("]
                            .iter()
                            .any(|k| t.starts_with(k))
                            && (t.contains(&format!("\"{d}\"")) || t.contains(&format!("'{d}'")))
                    })
                })?;
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
            let path = path.to_string_lossy().replace('\\', "/");
            if root.join(&path).is_file() {
                out.push(path);
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::assertion_coverage::{Language, change_at};

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
}
