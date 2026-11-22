//! A port of the mutant gem's mutators (`Mutant::Mutator::Node` and its
//! subclasses, mutant 0.17.1, light operators, no ignore patterns) to the
//! parser tree of `tree.rs`.
//!
//! Each function mirrors one mutator class and emits in the same order, so
//! the mutants come out as mutant lists them. A mutator returns the nodes
//! that replace its input; a parent puts each into a copy of itself
//! (`emit_child_update`). Like mutant's output sets, the list drops what
//! equals the input and what it already holds.
//!
//! `Parent` is mutant's `parent`: the mutator that asked for these
//! mutations, or none where mutant called `mutate(node:)` without one
//! (`Arguments`, `Kwargs`) or ran a sub-mutator (`Send`'s `Binary`). Some
//! rules read it (`left_op_assignment?`, a ternary under `return`, `super`
//! in a method with parameters), so it is kept exactly as mutant passes it.

use std::collections::HashSet;
use std::rc::Rc;

use super::tree::{Child, N};

/// The mutator that asked for a node's mutations, and where the node is in
/// its input.
pub(crate) struct Parent<'a> {
    pub node: &'a N,
    pub index: usize,
    pub up: Option<&'a Parent<'a>>,
}

/// mutant's output set: insertion order, no duplicates, never the input.
struct Out<'a> {
    input: &'a N,
    items: Vec<Rc<N>>,
    seen: HashSet<Rc<N>>,
}

impl<'a> Out<'a> {
    fn new(input: &'a N) -> Self {
        Self {
            input,
            items: Vec::new(),
            seen: HashSet::new(),
        }
    }

    fn emit(&mut self, node: Rc<N>) {
        if *node == *self.input || self.seen.contains(&node) {
            return;
        }
        self.seen.insert(node.clone());
        self.items.push(node);
    }

    /// `emit_type(*children)`.
    fn emit_type(&mut self, children: Vec<Child>) {
        let node = self.input.with_children(children);
        self.emit(node);
    }

    /// `emit_child_update(index, node)`.
    fn update(&mut self, index: usize, child: Child) {
        let mut children = self.input.children.clone();
        if index < children.len() {
            children[index] = child;
        }
        self.emit_type(children);
    }

    /// `delete_child(index)`: the input without that child.
    fn delete(&mut self, index: usize) {
        let mut children = self.input.children.clone();
        children.remove(index);
        self.emit_type(children);
    }

    fn done(self) -> Vec<Rc<N>> {
        self.items
    }
}

fn s(ty: &'static str, children: Vec<Child>) -> Rc<N> {
    N::new(ty, children)
}

fn sym(name: &str) -> Child {
    Child::Sym(name.to_owned())
}

fn node(value: Rc<N>) -> Child {
    Child::Node(value)
}

fn is(child: &Child, ty: &str) -> bool {
    child.node().is_some_and(|node| node.ty == ty)
}

fn nil() -> Rc<N> {
    s("nil", Vec::new())
}

fn raise() -> Rc<N> {
    s("send", vec![Child::Nil, sym("raise")])
}

/// Unparser's keywords: an implicit self call of one would read as the
/// keyword.
const KEYWORDS: &[&str] = &[
    "BEGIN",
    "END",
    "__ENCODING__",
    "__END__",
    "__FILE__",
    "alias",
    "and",
    "begin",
    "break",
    "case",
    "class",
    "def",
    "define",
    "defined?",
    "do",
    "else",
    "elsif",
    "end",
    "ensure",
    "false",
    "for",
    "if",
    "in",
    "module",
    "next",
    "nil",
    "not",
    "or",
    "redo",
    "rescue",
    "retry",
    "return",
    "self",
    "super",
    "then",
    "true",
    "undef",
    "unless",
    "until",
    "when",
    "while",
    "yield",
];

const METHOD_OPERATORS: &[&str] = &[
    "!", "!=", "!~", "%", "&", "*", "**", "+", "+@", "-", "-@", "/", "<", "<<", "<=", "<=>", "==",
    "===", "=~", ">", ">=", ">>", "[]", "[]=", "^", "|", "~@",
];

const UNARY_METHOD_OPERATORS: &[&str] = &["~@", "+@", "-@", "!"];

pub(crate) fn binary_method_operator(selector: &str) -> bool {
    METHOD_OPERATORS.contains(&selector)
        && !matches!(selector, "[]" | "[]=")
        && !UNARY_METHOD_OPERATORS.contains(&selector)
}

fn attribute_assignment(selector: &str) -> bool {
    !METHOD_OPERATORS.contains(&selector) && selector.ends_with('=')
}

/// `Mutant::Mutation::Operators::Light::SELECTOR_REPLACEMENTS`.
pub(crate) fn selector_replacements(selector: &str) -> &'static [&'static str] {
    match selector {
        "!=" => &["=="],
        "**" => &["*"],
        "%" => &["/"],
        "&" => &["|", "^"],
        "*" => &["/"],
        "+" => &["-"],
        "-" => &["+"],
        "/" => &["*"],
        "<" => &["==", "eql?", "equal?"],
        "<<" => &[">>"],
        "<=" => &["<", "==", "eql?", "equal?"],
        "===" => &["is_a?"],
        "=~" => &["match?"],
        ">" => &["==", "eql?", "equal?"],
        ">=" => &[">", "==", "eql?", "equal?"],
        ">>" => &["<<"],
        "^" => &["&", "|"],
        "|" => &["&", "^"],
        "__send__" => &["public_send"],
        "all?" => &["any?", "none?"],
        "any?" => &["all?", "empty?", "none?"],
        "append" => &["prepend"],
        "assoc" => &["rassoc"],
        "at" => &["fetch", "key?"],
        "bytes" => &["chars"],
        "capitalize!" => &["capitalize"],
        "chars" => &["bytes"],
        "chomp!" => &["chomp"],
        "chop!" => &["chop"],
        "chunk" => &["each"],
        "chunk_while" => &["each"],
        "collect" => &["each"],
        "collect!" => &["collect"],
        "collect_concat" => &["collect"],
        "compact!" => &["compact"],
        "ceil" => &["floor"],
        "count" => &["size", "length"],
        "delete!" => &["delete"],
        "delete_if" => &["reject"],
        "detect" => &["first", "last"],
        "downcase" => &["upcase"],
        "downcase!" => &["downcase"],
        "drop" => &["take"],
        "each_key" => &["each_value"],
        "each_cons" => &["each"],
        "each_slice" => &["each"],
        "each_with_index" => &["each"],
        "each_value" => &["each_key"],
        "each_with_object" => &["each"],
        "empty?" => &["any?"],
        "encode!" => &["encode"],
        "end_with?" => &["start_with?"],
        "even?" => &["odd?"],
        "fetch" => &["key?"],
        "filter" => &["reject"],
        "filter!" => &["filter"],
        "filter_map" => &["map"],
        "find" => &["first", "last"],
        "flat_map" => &["map"],
        "flatten!" => &["flatten"],
        "floor" => &["ceil"],
        "grep" => &["grep_v"],
        "grep_v" => &["grep"],
        "gsub" => &["sub"],
        "gsub!" => &["gsub"],
        "is_a?" => &["instance_of?"],
        "keep_if" => &["select"],
        "keys" => &["values"],
        "kind_of?" => &["instance_of?"],
        "lstrip!" => &["lstrip"],
        "map" => &["each"],
        "map!" => &["map"],
        "match" => &["match?"],
        "max" => &["first", "last", "min"],
        "max_by" => &["first", "last", "min_by"],
        "merge!" => &["merge"],
        "method" => &["public_method"],
        "min" => &["first", "last", "max"],
        "min_by" => &["first", "last", "max_by"],
        "negative?" => &["positive?"],
        "none?" => &["any?", "all?"],
        "odd?" => &["even?"],
        "pop" => &["shift"],
        "positive?" => &["negative?"],
        "pred" => &["succ"],
        "prepend" => &["append"],
        "push" => &["unshift"],
        "reject" => &["select"],
        "reject!" => &["reject"],
        "reverse!" => &["reverse"],
        "reverse_each" => &["each"],
        "reverse_map" => &["map", "each"],
        "reverse_merge" => &["merge"],
        "rotate!" => &["rotate"],
        "rstrip!" => &["rstrip"],
        "sample" => &["first", "last"],
        "scrub!" => &["scrub"],
        "rassoc" => &["assoc"],
        "select" => &["reject"],
        "select!" => &["select"],
        "send" => &["public_send", "__send__"],
        "shift" => &["pop"],
        "shuffle!" => &["shuffle"],
        "slice_after" => &["each"],
        "slice_before" => &["each"],
        "slice_when" => &["each"],
        "sort!" => &["sort"],
        "sort_by" => &["sort"],
        "sort_by!" => &["sort_by"],
        "squeeze!" => &["squeeze"],
        "start_with?" => &["end_with?"],
        "strip" => &["lstrip", "rstrip"],
        "strip!" => &["strip"],
        "sub!" => &["sub"],
        "succ" => &["pred"],
        "swapcase!" => &["swapcase"],
        "take" => &["drop"],
        "to_a" => &["to_ary"],
        "to_f" => &["to_i"],
        "to_h" => &["to_hash"],
        "to_i" => &["to_int"],
        "to_s" => &["to_str"],
        "tr!" => &["tr"],
        "tr_s!" => &["tr_s"],
        "transform_keys" => &["transform_values"],
        "transform_keys!" => &["transform_keys"],
        "transform_values" => &["transform_keys"],
        "transform_values!" => &["transform_values"],
        "unicode_normalize!" => &["unicode_normalize"],
        "uniq!" => &["uniq"],
        "unshift" => &["push"],
        "upcase" => &["downcase"],
        "upcase!" => &["upcase"],
        "values" => &["keys"],
        "values_at" => &["fetch_values"],
        "zero?" => &["nonzero?"],
        _ => &[],
    }
}

/// Mutations of `input`, asked for by `parent`.
pub(crate) fn mutate(input: &N, parent: Option<&Parent<'_>>) -> Vec<Rc<N>> {
    let mut out = Out::new(input);
    match input.ty {
        "send" => send(&mut out, parent),
        "csend" => {
            send(&mut out, parent);
            out.emit(input.with_type("send"));
        }
        "and_asgn" => {
            mutate_child(&mut out, 0, parent, |_| true);
            mutate_child(&mut out, 1, parent, |_| true);
            out.emit(input.with_type("or_asgn"));
        }
        "or_asgn" => {
            singletons(&mut out, parent);
            mutate_child(&mut out, 1, parent, |_| true);
            out.emit(input.with_type("and_asgn"));
        }
        "arg" | "kwarg" | "mlhs" | "const_pattern" | "nil" | "__ENCODING__" | "cbase"
        | "lambda" => {}
        "optarg" | "kwoptarg" => {
            let required = if input.ty == "optarg" { "arg" } else { "kwarg" };
            out.emit(s(required, vec![input.child(0).clone()]));
            mutate_child(&mut out, 1, parent, |_| true);
        }
        "args" => arguments(&mut out),
        "begin" => begin(&mut out, parent),
        "and" | "or" => binary(&mut out, parent),
        "block" => block(&mut out, parent),
        "block_pass" => {
            if input.node(0).is_some() {
                mutate_child(&mut out, 0, parent, |_| true);
                if let Some(argument) = input.node(0)
                    && argument.ty == "sym"
                    && let Some(name) = argument.child(0).sym()
                {
                    for method in selector_replacements(name) {
                        out.update(0, node(s("sym", vec![sym(method)])));
                    }
                }
            }
        }
        "break" | "yield" => {
            generic(&mut out, parent);
            singletons(&mut out, parent);
            for index in 0..input.children.len() {
                out.delete(index);
            }
        }
        "next" => {
            generic(&mut out, parent);
            singletons(&mut out, parent);
            for index in 0..input.children.len() {
                out.delete(index);
            }
            out.emit(input.with_type("break"));
        }
        "case" => case(&mut out, parent),
        "case_match" => case_match(&mut out, parent),
        "class" => {
            if input.node(2).is_some() {
                mutate_child(&mut out, 2, parent, |_| true);
            }
        }
        "module" | "sclass" => {
            if input.node(1).is_some() {
                mutate_child(&mut out, 1, parent, |_| true);
            }
        }
        "while" | "until" | "while_post" | "until_post" => conditional_loop(&mut out, parent),
        "const" => {
            let under_const = parent.is_some_and(|parent| parent.node.ty == "const");
            if !under_const {
                singletons(&mut out, parent);
            }
            let mut children = vec![Child::Nil];
            children.extend(input.children.iter().skip(1).cloned());
            out.emit_type(children);
            for index in 0..input.children.len() {
                if input.node(index).is_some() {
                    mutate_child(&mut out, index, parent, |_| true);
                }
            }
        }
        "def" => define(&mut out, parent, 1, 2),
        "defs" => define(&mut out, parent, 2, 3),
        "defined?" => {
            out.emit(nil());
            out.emit(s("true", Vec::new()));
            if let Some(expression) = input.node(0)
                && expression.ty == "ivar"
            {
                out.emit(s(
                    "send",
                    vec![
                        Child::Nil,
                        sym("instance_variable_defined?"),
                        node(s("sym", vec![expression.child(0).clone()])),
                    ],
                ));
            }
        }
        "dstr" | "dsym" | "xstr" => {
            singletons(&mut out, parent);
            for index in 0..input.children.len() {
                mutate_child(&mut out, index, parent, |mutation| mutation.ty == "begin");
            }
        }
        "ensure" => {
            if let Some(body) = input.node(0) {
                out.emit(body.clone());
                mutate_child(&mut out, 0, parent, |_| true);
            }
            if input.node(1).is_some() {
                mutate_child(&mut out, 1, parent, |_| true);
            }
        }
        "if_guard" | "unless_guard" => {
            mutate_child(&mut out, 0, parent, |_| true);
            out.emit_type(vec![node(s("true", Vec::new()))]);
            out.emit_type(vec![node(s("false", Vec::new()))]);
        }
        "if" => if_node(&mut out, parent),
        "in_pattern" => {
            mutate_child(&mut out, 0, parent, |_| true);
            if input.node(1).is_some() {
                mutate_child(&mut out, 1, parent, |_| true);
            }
            if input.node(2).is_some() {
                mutate_child(&mut out, 2, parent, |_| true);
            }
        }
        "index" => index(&mut out, parent, false),
        "indexasgn" => index(&mut out, parent, true),
        "itblock" => {
            singletons(&mut out, parent);
            mutate_child(&mut out, 0, parent, |mutation| mutation.ty == "send");
            mutate_child(&mut out, 2, parent, references_it);
        }
        "numblock" => {
            singletons(&mut out, parent);
            mutate_child(&mut out, 0, parent, |mutation| mutation.ty == "send");
        }
        "kwargs" => kwargs(&mut out),
        "kwbegin" => {
            generic(&mut out, parent);
            singletons(&mut out, parent);
        }
        "array" => array(&mut out, parent),
        "true" => out.emit(s("false", Vec::new())),
        "false" => out.emit(s("true", Vec::new())),
        "hash" => {
            singletons(&mut out, parent);
            out.emit_type(Vec::new());
            for index in 0..input.children.len() {
                mutate_child(&mut out, index, parent, |_| true);
                out.delete(index);
            }
        }
        "pair" => {
            mutate_child(&mut out, 0, parent, |mutation| {
                mutation.ty != "nil" || !mutation.children.is_empty()
            });
            mutate_child(&mut out, 1, parent, |_| true);
        }
        "int" => integer(&mut out, parent),
        "float" => {
            singletons(&mut out, parent);
            for value in [0.0f64, 1.0] {
                out.emit_type(vec![Child::Float(value.to_bits())]);
            }
            for (numerator, denominator) in [(0.0f64, 0.0f64), (-1.0, 0.0), (1.0, 0.0)] {
                out.emit(s(
                    "send",
                    vec![
                        node(s("float", vec![Child::Float(numerator.to_bits())])),
                        sym("/"),
                        node(s("float", vec![Child::Float(denominator.to_bits())])),
                    ],
                ));
            }
        }
        "rational" | "complex" => {
            singletons(&mut out, parent);
            for value in numeric_values(input) {
                out.emit_type(vec![Child::Raw(value)]);
            }
        }
        "str" => {
            singletons(&mut out, parent);
            out.emit(s("str", vec![Child::Str(Vec::new())]));
        }
        "sym" => {
            singletons(&mut out, parent);
            if let Some(name) = input.child(0).sym() {
                out.emit_type(vec![sym(&format!("{name}__mutant__"))]);
            }
        }
        "irange" | "erange" => {
            singletons(&mut out, parent);
            if input.node(0).is_some() {
                mutate_child(&mut out, 0, parent, |_| true);
            }
            if input.node(1).is_some() {
                let inverse = if input.ty == "irange" {
                    "erange"
                } else {
                    "irange"
                };
                out.emit(input.with_type(inverse));
                mutate_child(&mut out, 1, parent, |_| true);
            }
        }
        "regexp" => regexp(&mut out, parent),
        "regopt" => {
            for flag in ["i", "m"] {
                let children = input
                    .children
                    .iter()
                    .filter(|child| child.sym() != Some(flag))
                    .cloned()
                    .collect();
                out.emit_type(children);
            }
        }
        "masgn" => mutate_child(&mut out, 1, parent, |_| true),
        "match_alt" => {
            if let Some(left) = input.node(0) {
                out.emit(left.clone());
            }
            if let Some(right) = input.node(1) {
                out.emit(right.clone());
            }
            mutate_child(&mut out, 0, parent, |_| true);
            mutate_child(&mut out, 1, parent, |_| true);
        }
        "match_current_line" => {
            singletons(&mut out, parent);
            mutate_child(&mut out, 0, parent, |_| true);
        }
        "match_pattern_p" => {
            out.emit(s("false", Vec::new()));
            mutate_child(&mut out, 0, parent, |_| true);
            mutate_child(&mut out, 1, parent, |_| true);
        }
        "gvar" | "cvar" | "ivar" | "lvar" | "self" => singletons(&mut out, parent),
        "gvasgn" | "cvasgn" | "ivasgn" | "lvasgn" => {
            if input.node(1).is_some() {
                mutate_child(&mut out, 1, parent, |_| true);
            }
        }
        "nth_ref" => {
            if let Child::Int(number) = input.child(0) {
                if *number != 1 {
                    out.update(0, Child::Int(number - 1));
                }
                out.update(0, Child::Int(number + 1));
            }
        }
        "op_asgn" => op_asgn(&mut out, parent),
        "procarg0" => {
            for index in 0..input.children.len() {
                mutate_child(&mut out, index, parent, |_| true);
            }
        }
        "resbody" => {
            if input.node(2).is_some() {
                mutate_child(&mut out, 2, parent, |_| true);
            }
        }
        "rescue" => rescue(&mut out, parent),
        "return" => {
            singletons(&mut out, parent);
            if let Some(value) = input.node(0) {
                out.emit(value.clone());
                mutate_child(&mut out, 0, parent, |_| true);
            }
        }
        "splat" => {
            singletons(&mut out, parent);
            mutate_child(&mut out, 0, parent, |_| true);
            if let Some(expression) = input.node(0) {
                out.emit(expression.clone());
            }
        }
        "super" => {
            singletons(&mut out, parent);
            if !input.children.is_empty() {
                out.emit(s("zsuper", Vec::new()));
            }
            for index in 0..input.children.len() {
                mutate_child(&mut out, index, parent, |_| true);
                out.delete(index);
            }
        }
        "when" => {
            let body_index = input.children.len() - 1;
            if input.node(body_index).is_some() {
                mutate_child(&mut out, body_index, parent, |_| true);
            } else {
                out.update(body_index, node(raise()));
            }
            let conditions = input.children.len() - 1;
            for index in 0..conditions {
                if conditions > 1 {
                    out.delete(index);
                }
                mutate_child(&mut out, index, parent, |_| true);
            }
        }
        "zsuper" => {
            singletons(&mut out, parent);
            if enclosing_method_has_arguments(parent) {
                out.emit(s("super", Vec::new()));
            }
        }
        _ => generic(&mut out, parent),
    }
    out.done()
}

/// `mutate_child(index)`: each mutation of that child the filter keeps, in
/// a copy of the input.
fn mutate_child(
    out: &mut Out<'_>,
    index: usize,
    parent: Option<&Parent<'_>>,
    keep: impl Fn(&N) -> bool,
) {
    let input = out.input;
    let Some(child) = input.node(index) else {
        return;
    };
    let here = Parent {
        node: input,
        index,
        up: parent,
    };
    // mutant's `parent` for the child is this mutator, whose own parent is
    // ours.
    for mutation in mutate(child, Some(&here)) {
        if keep(&mutation) {
            out.update(index, node(mutation));
        }
    }
}

fn generic(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    for index in 0..out.input.children.len() {
        if out.input.node(index).is_some() {
            mutate_child(out, index, parent, |_| true);
        }
    }
}

/// `emit_singletons`: `nil`, except as the target of an operator
/// assignment.
fn singletons(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    if !left_op_assignment(parent) {
        out.emit(nil());
    }
}

fn left_op_assignment(parent: Option<&Parent<'_>>) -> bool {
    parent.is_some_and(|parent| {
        matches!(parent.node.ty, "op_asgn" | "or_asgn" | "and_asgn") && parent.index == 0
    })
}

fn send(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    singletons(out, parent);
    let selector = out.input.child(1).sym().unwrap_or_default().to_owned();
    if binary_method_operator(&selector) {
        send_binary(out);
    } else if attribute_assignment(&selector) {
        normal_dispatch(out, parent, true);
        let reader = selector[..selector.len() - 1].to_owned();
        out.emit_type(vec![out.input.child(0).clone(), Child::Sym(reader)]);
    } else {
        normal_dispatch(out, parent, false);
    }
}

fn normal_dispatch(out: &mut Out<'_>, parent: Option<&Parent<'_>>, attribute: bool) {
    let input = out.input;
    let receiver = input.child(0).clone();
    let selector = input.child(1).sym().unwrap_or_default().to_owned();
    let arguments: Vec<Child> = input.children.iter().skip(2).cloned().collect();
    // `run(AttributeAssignment)` has no parent.
    let own_parent = if attribute { None } else { parent };
    // emit_naked_receiver
    if let Some(receiver) = receiver.node()
        && !left_op_assignment(own_parent)
    {
        out.emit(receiver.clone());
    }
    // emit_selector_replacement
    for replacement in selector_replacements(&selector) {
        out.update(1, sym(replacement));
    }
    // emit_reduce_to_sum_mutation
    if selector == "reduce"
        && let Some(reducer) = arguments.last().and_then(Child::node)
    {
        let plus = s("sym", vec![sym("+")]);
        let pass = s("block_pass", vec![node(plus.clone())]);
        if **reducer == *plus || **reducer == *pass {
            if arguments.len() > 1 {
                out.emit_type(vec![receiver.clone(), sym("sum"), arguments[0].clone()]);
            } else {
                out.emit_type(vec![receiver.clone(), sym("sum")]);
            }
        }
    }
    // emit_start_end_with_mutations
    if matches!(selector.as_str(), "=~" | "match" | "match?")
        && arguments.len() == 1
        && let Some(argument) = arguments[0].node()
        && argument.ty == "regexp"
        && let Some(body) = regexp_body(argument)
    {
        match super::regexp::anchored_literal(&body) {
            Some(super::regexp::Anchored::Start(text)) => out.emit_type(vec![
                receiver.clone(),
                sym("start_with?"),
                node(s("str", vec![Child::Str(text.into_bytes())])),
            ]),
            Some(super::regexp::Anchored::End(text)) => out.emit_type(vec![
                receiver.clone(),
                sym("end_with?"),
                node(s("str", vec![Child::Str(text.into_bytes())])),
            ]),
            None => {}
        }
    }
    // emit_predicate_mutations
    if selector.ends_with('?') && selector != "defined?" {
        out.emit(s("true", Vec::new()));
        out.emit(s("false", Vec::new()));
    }
    // emit_array_mutation
    let kernel = s("const", vec![Child::Nil, sym("Kernel")]);
    if selector == "Array"
        && (receiver.is_nil()
            || receiver
                .node()
                .is_some_and(|receiver| **receiver == *kernel))
    {
        out.emit(s("array", arguments.clone()));
    }
    // emit_integer_mutation
    if selector == "to_i" {
        out.emit(s(
            "send",
            vec![Child::Nil, sym("Integer"), receiver.clone()],
        ));
    }
    // emit_empty_collection_mutation
    match selector.as_str() {
        "to_a" | "to_ary" => out.emit(s("array", Vec::new())),
        "to_h" | "to_hash" => out.emit(s("hash", Vec::new())),
        "to_s" | "to_str" => out.emit(s("str", vec![Child::Str(Vec::new())])),
        _ => {}
    }
    // emit_static_send
    if matches!(selector.as_str(), "__send__" | "send" | "public_send")
        && let Some(first) = arguments.first().and_then(Child::node)
        && first.ty == "sym"
        && let Some(name) = first.child(0).sym()
    {
        let mut children = vec![receiver.clone(), sym(name)];
        children.extend(arguments.iter().skip(1).cloned());
        out.emit(N::new(input.ty, children));
    }
    // emit_const_get_mutation
    if selector == "const_get"
        && let Some(first) = arguments.first().and_then(Child::node)
        && first.ty == "sym"
        && let Some(name) = first.child(0).sym()
    {
        out.emit(s("const", vec![receiver.clone(), sym(name)]));
    }
    // emit_dig_mutation
    if selector == "dig" && !arguments.is_empty() {
        let fetch = s(
            "send",
            vec![receiver.clone(), sym("fetch"), arguments[0].clone()],
        );
        if arguments.len() == 1 {
            out.emit(fetch);
        } else {
            let mut children = vec![node(fetch), sym("dig")];
            children.extend(arguments.iter().skip(1).cloned());
            out.emit(s("send", children));
        }
    }
    // emit_double_negation_mutation
    if selector == "!"
        && let Some(inner) = receiver.node()
        && inner.ty == "send"
        && inner.child(1).sym() == Some("!")
    {
        out.emit_child_or_nil(inner.child(0).clone());
    }
    // emit_lambda_mutation
    let naked_proc = receiver.is_nil() && selector == "proc";
    let proc_new = selector == "new"
        && receiver.node().is_some_and(|receiver| {
            receiver.ty == "const" && receiver.child(1).sym() == Some("Proc")
        });
    if naked_proc || proc_new {
        out.emit(s("send", vec![Child::Nil, sym("lambda")]));
    }
    // emit_argument_propagation
    if arguments.len() == 1
        && let Some(argument) = arguments[0].node()
        && !matches!(
            argument.ty,
            "kwargs" | "forwarded_args" | "forwarded_restarg"
        )
        && !matches!(argument.ty, "splat" | "restarg" | "block_pass")
    {
        out.emit(argument.clone());
    }
    // emit_receiver_selector_mutations
    if let Some(receiver_node) = receiver.node()
        && receiver_node.ty == "const"
        && (receiver_node.child(0).is_nil() || is(receiver_node.child(0), "cbase"))
        && receiver_node.child(1).sym() == Some("Date")
        && selector == "parse"
    {
        for replacement in [
            "jd",
            "civil",
            "strptime",
            "iso8601",
            "rfc3339",
            "xmlschema",
            "rfc2822",
            "rfc822",
            "httpdate",
            "jisx0301",
        ] {
            out.update(1, sym(replacement));
        }
    }
    // mutate_receiver
    if let Some(receiver_node) = receiver.node() {
        if receiver_node.ty == "self"
            && !(KEYWORDS.contains(&selector.as_str()) || attribute_assignment(&selector))
        {
            out.update(0, Child::Nil);
        }
        if !UNARY_METHOD_OPERATORS.contains(&selector.as_str()) && receiver_node.ty != "nil" {
            out.update(0, node(s("self", Vec::new())));
        }
        mutate_child(out, 0, own_parent, |mutation| {
            mutation.ty != "nil" || !mutation.children.is_empty()
        });
    }
    // mutate_arguments
    if attribute {
        for index in 2..input.children.len() {
            mutate_child(out, index, own_parent, |_| true);
        }
    } else {
        out.emit_type(vec![receiver.clone(), sym(&selector)]);
        for index in 2..input.children.len() {
            mutate_child(out, index, own_parent, |mutation| mutation.ty != "begin");
            out.delete(index);
        }
    }
}

impl Out<'_> {
    /// `emit(node)` of a child that may be nil (a receiverless `!!x`).
    fn emit_child_or_nil(&mut self, child: Child) {
        if let Child::Node(node) = child {
            self.emit(node);
        }
    }
}

/// `Send::Binary`, run without a parent.
fn send_binary(out: &mut Out<'_>) {
    let input = out.input;
    let left = input.child(0).clone();
    let operator = input.child(1).sym().unwrap_or_default().to_owned();
    let right = input.child(2).clone();
    if let Child::Node(left) = &left {
        out.emit(left.clone());
    }
    mutate_child(out, 0, None, |_| true);
    for replacement in selector_replacements(&operator) {
        let swap = matches!((operator.as_str(), *replacement), ("*", "/") | ("/", "*"));
        if swap && multiplicative_identity(&right) {
            continue;
        }
        out.update(1, sym(replacement));
    }
    if let Child::Node(right) = &right {
        out.emit(right.clone());
    }
    mutate_child(out, 2, None, |_| true);
    if operator == "!=" {
        for method in ["eql?", "equal?"] {
            let call = s("send", vec![left.clone(), sym(method), right.clone()]);
            out.emit(s("send", vec![node(call), sym("!")]));
        }
    }
}

fn multiplicative_identity(child: &Child) -> bool {
    let Some(value) = child.node() else {
        return false;
    };
    match (value.ty, value.child(0)) {
        ("int", Child::Int(number)) => *number == 1 || *number == -1,
        ("float", Child::Float(bits)) => {
            let number = f64::from_bits(*bits);
            number == 1.0 || number == -1.0
        }
        _ => false,
    }
}

fn arguments(out: &mut Out<'_>) {
    let input = out.input;
    for (index, child) in input.children.iter().enumerate() {
        let Some(child) = child.node() else {
            continue;
        };
        for mutation in mutate(child, None) {
            let invalid = mutation.ty == "arg"
                && input.children[..index]
                    .iter()
                    .any(|earlier| is(earlier, "optarg"));
            if !invalid {
                out.update(index, node(mutation));
            }
        }
    }
    for (index, child) in input.children.iter().enumerate() {
        if let Some(mlhs) = child.node()
            && mlhs.ty == "mlhs"
        {
            let mut children = input.children.clone();
            children.remove(index);
            for (offset, inner) in mlhs.children.iter().enumerate() {
                children.insert(index + offset, inner.clone());
            }
            out.emit_type(children);
        }
    }
    if input.children.len() == 1
        && let Some(procarg) = input.node(0)
        && procarg.ty == "procarg0"
        && procarg.children.len() > 1
    {
        out.emit_type(procarg.children.clone());
    }
}

const ASSIGN_NODES: &[&str] = &[
    "arg",
    "kwarg",
    "kwoptarg",
    "kwrestarg",
    "lvasgn",
    "optarg",
    "restarg",
];
const RESET_NODES: &[&str] = &["module", "class", "sclass", "def", "defs"];

fn contains_lvar_assignment(value: &N) -> bool {
    if ASSIGN_NODES.contains(&value.ty) {
        return true;
    }
    if RESET_NODES.contains(&value.ty) {
        return false;
    }
    value
        .children
        .iter()
        .filter_map(Child::node)
        .any(|child| contains_lvar_assignment(child))
}

fn begin(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    for index in 0..input.children.len() {
        mutate_child(out, index, parent, |_| true);
        let mut rest = input.children.clone();
        let removed = rest.remove(index);
        if removed
            .node()
            .is_some_and(|removed| contains_lvar_assignment(removed))
        {
            continue;
        }
        match rest.len() {
            0 => {}
            1 => {
                if let Child::Node(one) = rest.pop().expect("one") {
                    out.emit(one);
                }
            }
            _ => out.emit_type(rest),
        }
    }
}

fn binary(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    let left_lvasgn = is(input.child(0), "lvasgn");
    if !left_lvasgn {
        singletons(out, parent);
    }
    if let Some(left) = input.node(0) {
        out.emit(left.clone());
    }
    if !left_lvasgn && let Some(right) = input.node(1) {
        out.emit(right.clone());
    }
    let inverse = if input.ty == "and" { "or" } else { "and" };
    out.emit(input.with_type(inverse));
    mutate_child(out, 0, parent, |mutation| {
        !matches!(mutation.ty, "irange" | "erange") || !mutation.child(1).is_nil()
    });
    mutate_child(out, 1, parent, |_| true);
}

fn block(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    singletons(out, parent);
    let send = input.node(0).cloned();
    let lambda = send.as_ref().is_some_and(|send| send.ty == "lambda");
    if let Some(send) = &send
        && !lambda
    {
        out.emit(send.clone());
    }
    if let Some(send) = &send
        && send.ty == "send"
        && let Some(receiver) = send.node(0)
    {
        out.emit(receiver.clone());
    }
    mutate_child(out, 0, parent, |mutation| {
        mutation.ty == "send"
            && !mutation
                .children
                .iter()
                .skip(2)
                .last()
                .is_some_and(|last| is(last, "block_pass"))
    });
    mutate_child(out, 1, parent, |_| true);
    let loop_call = s("send", vec![Child::Nil, sym("loop")]);
    let unconditional_loop = send.as_ref().is_some_and(|send| **send == *loop_call);
    if !unconditional_loop {
        out.update(2, Child::Nil);
    }
    out.update(2, node(raise()));
    let Some(body) = input.node(2).cloned() else {
        return;
    };
    let empty_arguments = input
        .node(1)
        .is_some_and(|arguments| arguments.children.is_empty());
    if empty_arguments && !has_control(&body) {
        out.emit(body.clone());
    }
    mutate_child(out, 2, parent, |mutation| {
        !(mutation.ty == "nil" && mutation.children.is_empty() && unconditional_loop)
    });
    if !lambda
        && body.ty == "send"
        && empty_arguments
        && let Some(send) = &send
    {
        let mut children = vec![node(send.clone()), body.child(1).clone()];
        children.extend(body.children.iter().skip(2).cloned());
        out.emit(s("send", children));
    }
}

fn has_control(value: &N) -> bool {
    matches!(value.ty, "break" | "next")
        || value
            .children
            .iter()
            .filter_map(Child::node)
            .any(|child| has_control(child))
}

fn references_it(value: &N) -> bool {
    let it = s("lvar", vec![sym("it")]);
    if *value == *it {
        return true;
    }
    let owns_it = matches!(value.ty, "block" | "itblock" | "def" | "defs");
    let children: Vec<&Child> = if owns_it {
        value.children.iter().take(1).collect()
    } else {
        value.children.iter().collect()
    };
    children
        .into_iter()
        .filter_map(Child::node)
        .any(|child| references_it(child))
}

fn case(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    singletons(out, parent);
    if input.node(0).is_some() {
        mutate_child(out, 0, parent, |_| true);
    }
    let length = input.children.len();
    let whens: Vec<usize> = (1..length.saturating_sub(1)).collect();
    let one = whens.len() == 1;
    for index in whens {
        mutate_child(out, index, parent, |_| true);
        if !one {
            out.delete(index);
        }
    }
    let last = length - 1;
    if input.node(last).is_some() {
        mutate_child(out, last, parent, |_| true);
        out.update(last, Child::Nil);
    }
}

fn case_match(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    mutate_child(out, 0, parent, |_| true);
    let length = input.children.len();
    let patterns: Vec<usize> = (1..length.saturating_sub(1)).collect();
    let one = patterns.len() == 1;
    for index in patterns {
        mutate_child(out, index, parent, |_| true);
        if !one {
            out.delete(index);
        }
    }
    let last = length - 1;
    if input.node(last).is_some() {
        mutate_child(out, last, parent, |_| true);
        out.update(last, Child::Nil);
    }
}

fn conditional_loop(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    singletons(out, parent);
    mutate_child(out, 0, parent, |_| true);
    let inverse = match input.ty {
        "until" => "while",
        "until_post" => "while_post",
        "while" => "until",
        _ => "until_post",
    };
    out.emit(input.with_type(inverse));
    if input.ty.ends_with("_post") {
        mutate_child(out, 1, parent, |mutation| mutation.ty == "kwbegin");
        out.update(1, node(s("kwbegin", vec![node(raise())])));
    } else {
        if input.node(1).is_some() {
            mutate_child(out, 1, parent, |_| true);
        }
        out.update(1, Child::Nil);
        out.update(1, node(raise()));
    }
}

fn define(
    out: &mut Out<'_>,
    parent: Option<&Parent<'_>>,
    arguments_index: usize,
    body_index: usize,
) {
    let input = out.input;
    mutate_child(out, arguments_index, parent, |_| true);
    let body = input.node(body_index).cloned();
    if let Some(arguments) = input.node(arguments_index) {
        for argument in &arguments.children {
            let Some(argument) = argument.node() else {
                continue;
            };
            let used = argument
                .child(0)
                .sym()
                .is_some_and(|name| !name.starts_with('_'));
            if argument.ty == "optarg" && used {
                let assignment = s("lvasgn", argument.children.clone());
                let prepended = match &body {
                    Some(body) => s("begin", vec![node(assignment), node(body.clone())]),
                    None => assignment,
                };
                out.update(body_index, node(prepended));
            }
        }
    }
    out.update(body_index, node(raise()));
    out.update(body_index, node(s("zsuper", Vec::new())));
    if let Some(body) = &body {
        let returned = if body.ty == "begin" {
            body.children.last().and_then(Child::node).cloned()
        } else {
            Some(body.clone())
        };
        let default = returned.and_then(|returned| match returned.ty {
            "array" => Some(s("array", Vec::new())),
            "hash" => Some(s("hash", Vec::new())),
            "str" => Some(s("str", vec![Child::Str(Vec::new())])),
            "int" => Some(s("int", vec![Child::Int(0)])),
            "float" => Some(s("float", vec![Child::Float(0f64.to_bits())])),
            _ => None,
        });
        if let Some(default) = default {
            out.update(body_index, node(default));
        }
    }
    if body.is_none() {
        return;
    }
    out.update(body_index, Child::Nil);
    mutate_child(out, body_index, parent, |_| true);
}

fn if_node(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    let ternary =
        parent.is_some_and(|parent| matches!(parent.node.ty, "return" | "next" | "break"));
    let condition = input.child(0).clone();
    let then_branch = input.child(1).clone();
    let else_branch = input.child(2).clone();
    let emit_if = |out: &mut Out<'_>, condition: Child, then_branch: Child, else_branch: Child| {
        if ternary && (then_branch.is_nil() || else_branch.is_nil()) {
            return;
        }
        out.emit_type(vec![condition, then_branch, else_branch]);
    };
    // Every emit goes through If's own `emit_type`, child updates too: under
    // `return`, `next` or `break`, nothing with a branch missing.
    let blocked = ternary && (then_branch.is_nil() || else_branch.is_nil());
    singletons(out, parent);
    if !blocked {
        mutate_child(out, 0, parent, |_| true);
    }
    emit_if(
        out,
        node(s("true", Vec::new())),
        then_branch.clone(),
        else_branch.clone(),
    );
    emit_if(
        out,
        node(s("false", Vec::new())),
        then_branch.clone(),
        else_branch.clone(),
    );
    if !else_branch.is_nil() {
        emit_if(out, condition.clone(), else_branch.clone(), Child::Nil);
    }
    if let Some(then_node) = then_branch.node() {
        out.emit(then_node.clone());
        if !blocked {
            mutate_child(out, 1, parent, |_| true);
        }
        emit_if(out, condition.clone(), then_branch.clone(), Child::Nil);
    }
    if let Some(else_node) = else_branch.node() {
        out.emit(else_node.clone());
        if !blocked {
            mutate_child(out, 2, parent, |_| true);
        }
        emit_if(out, condition, Child::Nil, else_branch.clone());
    }
}

fn index(out: &mut Out<'_>, parent: Option<&Parent<'_>>, assign: bool) {
    let input = out.input;
    let left_op = left_op_assignment(parent);
    let length = input.children.len();
    // `children_indices` of the index range.
    let range: Vec<usize> = if assign && !left_op {
        (1..length.saturating_sub(1)).collect()
    } else {
        (1..length).collect()
    };
    let receiver = input.child(0).clone();
    let indices: Vec<Child> = range
        .iter()
        .map(|index| input.children[*index].clone())
        .collect();
    singletons(out, parent);
    mutate_child(out, 0, parent, |mutation| {
        mutation.ty != "nil" || !mutation.children.is_empty()
    });
    let mut with_self = vec![node(s("self", Vec::new()))];
    with_self.extend(input.children.iter().skip(1).cloned());
    out.emit_type(with_self);
    if let Child::Node(receiver) = &receiver {
        out.emit(receiver.clone());
    }
    if !left_op {
        for selector in ["at", "fetch", "key?"] {
            let mut children = vec![receiver.clone(), sym(selector)];
            children.extend(indices.iter().cloned());
            out.emit(s("send", children));
        }
    }
    if indices.len() == 1
        && let Some(range) = indices[0].node()
        && range.ty == "irange"
        && range
            .node(1)
            .is_some_and(|ending| **ending == *s("int", vec![Child::Int(-1)]))
    {
        out.emit(s(
            "send",
            vec![receiver.clone(), sym("drop"), range.child(0).clone()],
        ));
    }
    for index in &range {
        out.delete(*index);
        mutate_child(out, *index, parent, |_| true);
    }
    if assign {
        if left_op {
            return;
        }
        let mut read = vec![receiver.clone()];
        read.extend(indices.iter().cloned());
        out.emit(s("index", read));
        if let Some(value) = input.children.last().and_then(Child::node) {
            out.emit(value.clone());
        }
        mutate_child(out, length - 1, parent, |_| true);
    }
}

fn kwargs(out: &mut Out<'_>) {
    let input = out.input;
    for index in 0..input.children.len() {
        let mut children = input.children.clone();
        children.remove(index);
        if !children.is_empty() {
            out.emit_type(children);
        }
    }
    for (index, child) in input.children.iter().enumerate() {
        let Some(child) = child.node() else {
            continue;
        };
        for mutation in mutate(child, None) {
            let forbidden = mutation.ty == "pair"
                && mutation
                    .node(0)
                    .is_some_and(|key| matches!(key.ty, "nil" | "self"));
            if !forbidden {
                out.update(index, node(mutation));
            }
        }
    }
}

fn array(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    singletons(out, parent);
    out.emit_type(Vec::new());
    for index in 0..input.children.len() {
        out.delete(index);
        mutate_child(out, index, parent, |_| true);
    }
    if input.children.len() == 1
        && let Some(first) = input.node(0)
        && first.ty != "splat"
    {
        out.emit(first.clone());
    }
}

/// mutant's integer overflow zones: the literal also becomes the safe prime
/// of the next integer width above it.
const OVERFLOW_ZONES: &[(u128, &str)] = &[
    (1 << 7, "167"),
    (1 << 8, "467"),
    (1 << 15, "55487"),
    (1 << 16, "108503"),
    (1 << 31, "2667278543"),
    (1 << 32, "7980081959"),
    (1 << 63, "15508464536481899903"),
];

fn integer(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    singletons(out, parent);
    let Child::Int(value) = out.input.child(0).clone() else {
        return;
    };
    for each in [0, 1, value + 1, value - 1] {
        out.emit_type(vec![Child::Int(each)]);
    }
    let absolute = value.unsigned_abs();
    if let Some((_, sentinel)) = OVERFLOW_ZONES
        .iter()
        .find(|(boundary, _)| absolute < *boundary)
    {
        let child = match sentinel.parse::<i128>() {
            Ok(number) => Child::Int(number),
            Err(_) => Child::BigInt((*sentinel).to_owned()),
        };
        out.emit_type(vec![child]);
    }
}

/// A rational's or imaginary's mutations' values, as `inspect` prints them:
/// 0, 1, and one more and one less than the literal.
fn numeric_values(input: &N) -> Vec<String> {
    let Child::Raw(text) = input.child(0) else {
        unreachable!("a rational or imaginary holds its inspected value");
    };
    if input.ty == "rational" {
        // `(n/d)`, reduced: n ± d over d stays reduced.
        let (numerator, denominator) = text
            .trim_start_matches('(')
            .trim_end_matches(')')
            .split_once('/')
            .expect("a rational's text");
        let numerator = numerator.parse::<i128>().expect("a numerator");
        let denominator = denominator.parse::<i128>().expect("a denominator");
        return [
            (0, 1),
            (1, 1),
            (numerator + denominator, denominator),
            (numerator - denominator, denominator),
        ]
        .into_iter()
        .map(|(numerator, denominator)| format!("({numerator}/{denominator})"))
        .collect();
    }
    let imaginary = text
        .trim_start_matches("(0+")
        .trim_end_matches("i)")
        .parse::<i128>()
        .expect("an imaginary's text");
    // 0i, 1i, and the literal plus and minus 1i.
    [0, 1, imaginary + 1, imaginary - 1]
        .into_iter()
        .map(|value| {
            if value < 0 {
                format!("(0-{}i)", -value)
            } else {
                format!("(0+{value}i)")
            }
        })
        .collect()
}

/// The regexp's source where every part is a string (no interpolation).
pub(crate) fn regexp_body(regexp: &N) -> Option<String> {
    let mut body = Vec::new();
    for child in &regexp.children[..regexp.children.len().saturating_sub(1)] {
        let part = child.node()?;
        if part.ty != "str" {
            return None;
        }
        let Child::Str(bytes) = part.child(0) else {
            return None;
        };
        body.extend_from_slice(bytes);
    }
    String::from_utf8(body).ok()
}

fn regexp(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    let options = input.children.last().cloned().unwrap_or(Child::Nil);
    if let Some(body) = regexp_body(input) {
        for mutated in super::regexp::mutations(&body) {
            out.emit_type(vec![
                node(s("str", vec![Child::Str(mutated.into_bytes())])),
                options.clone(),
            ]);
        }
    }
    if parent.is_none() {
        out.emit(nil());
    }
    for (index, child) in input.children.iter().enumerate() {
        if !is(child, "str") && child.node().is_some() {
            mutate_child(out, index, parent, |_| true);
        }
    }
    out.emit_type(vec![options.clone()]);
    out.emit_type(vec![
        node(s("str", vec![Child::Str(b"nomatch\\A".to_vec())])),
        options,
    ]);
}

fn op_asgn(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    mutate_child(out, 0, parent, |mutation| mutation.ty != "self");
    if let Some(left) = input.node(0)
        && left.ty == "send"
        && let Some(receiver) = left.node(0)
        && receiver.ty == "ivar"
    {
        out.update(0, node(s("ivasgn", receiver.children.clone())));
    }
    let operator = input.child(1).sym().unwrap_or_default();
    let swaps: &[&str] = match operator {
        "+" => &["-"],
        "-" => &["+"],
        "*" => &["/"],
        "/" => &["*"],
        "%" => &["/"],
        "**" => &["*"],
        "&" => &["|"],
        "|" => &["&", "^"],
        "^" => &["&", "|"],
        "<<" => &[">>"],
        ">>" => &["<<"],
        _ => &[],
    };
    for swap in swaps {
        out.update(1, sym(swap));
    }
    mutate_child(out, 2, parent, |_| true);
}

fn rescue(out: &mut Out<'_>, parent: Option<&Parent<'_>>) {
    let input = out.input;
    let standalone = parent.is_none();
    let length = input.children.len();
    let body = input.node(0).cloned();
    let concat = |out: &mut Out<'_>, child: Rc<N>| match &body {
        Some(body) => out.emit(s("begin", vec![node(body.clone()), node(child)])),
        None => out.emit(child),
    };
    if let Some(body) = &body {
        mutate_child(out, 0, parent, |_| true);
        out.emit(body.clone());
    }
    let rescues: Vec<usize> = (1..length.saturating_sub(1)).collect();
    for index in &rescues {
        mutate_child(out, *index, parent, |_| true);
        if let Some(resbody) = input.node(*index)
            && let Some(handler) = resbody.node(2)
            && resbody.child(1).is_nil()
        {
            concat(out, handler.clone());
        }
    }
    if rescues.len() > 1 {
        for index in &rescues {
            out.delete(*index);
        }
    }
    if standalone {
        for index in &rescues {
            if let Some(handler) = input.node(*index).and_then(|resbody| resbody.node(2)) {
                out.emit(handler.clone());
            }
        }
    }
    if let Some(else_body) = input.node(length - 1).cloned() {
        mutate_child(out, length - 1, parent, |_| true);
        concat(out, else_body);
    }
    if standalone {
        out.emit(nil());
    }
}

fn enclosing_method_has_arguments(parent: Option<&Parent<'_>>) -> bool {
    let mut current = parent;
    while let Some(each) = current {
        let arguments = match each.node.ty {
            "def" => each.node.node(1),
            "defs" => each.node.node(2),
            _ => {
                current = each.up;
                continue;
            }
        };
        return arguments.is_some_and(|arguments| !arguments.children.is_empty());
    }
    false
}
