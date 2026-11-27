//! The mutants PIT's `DEFAULTS` group makes of a Java or Kotlin file, in
//! source form, enumerated from the program as written without running PIT.
//!
//! PIT mutates bytecode. Each of its operators is a change to one instruction
//! (a conditional jump, an arithmetic opcode, an `IINC`, a return, a call),
//! and the compiler decides which instructions a line becomes. So the rules
//! here are written in terms of what javac and kotlinc emit for the code as
//! written: a comparison is a jump wherever it is, a boolean variable is a
//! jump only where it is a condition, `i++` on a local `int` is an `IINC`
//! (Increments) and on a field an `IADD` (Math), `return a < b` already
//! returns `false` on one path, and so on. PIT's default filters are applied
//! in the same terms: static initializers, for-loop counters, assertions,
//! logging lines, `switch` on strings and enums, returns that already return
//! the replacement.
//!
//! Where the compiler's choice depends on a type the source does not spell
//! out, the type is inferred from declarations in the file and a project-wide
//! index of method declarations. A guess that names a mutant PIT would not
//! make either fails to compile in the strong copy, and is reported
//! `unviable` there, or is a mutant PIT cannot see (a non-void call removed);
//! `scripts/pit-parity.py` reports both kinds against PIT's own list.
//!
//! # Descriptors
//!
//! An id is `file:startLine:startCol-endLine:endCol:Mutator:descriptor`, with
//! PIT's mutator names less `Mutator`:
//!
//! | Mutator | Descriptor |
//! | --- | --- |
//! | `ConditionalsBoundary` | the new operator: `<=` `<` `>=` `>` |
//! | `NegateConditionals` | `negated` |
//! | `Math` | the new operator: `-` `+` `/` `*` `|` `&` `>>` `<<`, `-=` and the like for a compound assignment, `--` `++` for `x++` on what is not a local `int` |
//! | `Increments` | the new operator: `--` `++` `-=` `+=` |
//! | `InvertNegs` | `removed` |
//! | `VoidMethodCall` | `removed` |
//! | `PrimitiveReturns` | `0` |
//! | `BooleanTrueReturnVals`, `BooleanFalseReturnVals` | `true`, `false` |
//! | `NullReturnVals` | `null` |
//! | `EmptyObjectReturnVals` | the empty value: `""` `0` `emptyList` `emptySet` `emptyMap` `Optional.empty` `Stream.empty` |

use std::collections::{BTreeSet, HashMap};

use tree_sitter::Node;

use crate::jvm_instrumenter::JvmLanguage;

/// The mutant set every JVM report names: PIT's version and its group.
pub const MUTANT_SET: &str = "pitest@1.17.1";

/// PIT's mutators in its `DEFAULTS` group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Mutator {
    ConditionalsBoundary,
    NegateConditionals,
    Math,
    Increments,
    InvertNegs,
    VoidMethodCall,
    EmptyObjectReturnVals,
    BooleanFalseReturnVals,
    BooleanTrueReturnVals,
    NullReturnVals,
    PrimitiveReturns,
}

impl Mutator {
    pub const ALL: [Mutator; 11] = [
        Mutator::ConditionalsBoundary,
        Mutator::NegateConditionals,
        Mutator::Math,
        Mutator::Increments,
        Mutator::InvertNegs,
        Mutator::VoidMethodCall,
        Mutator::EmptyObjectReturnVals,
        Mutator::BooleanFalseReturnVals,
        Mutator::BooleanTrueReturnVals,
        Mutator::NullReturnVals,
        Mutator::PrimitiveReturns,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Mutator::ConditionalsBoundary => "ConditionalsBoundary",
            Mutator::NegateConditionals => "NegateConditionals",
            Mutator::Math => "Math",
            Mutator::Increments => "Increments",
            Mutator::InvertNegs => "InvertNegs",
            Mutator::VoidMethodCall => "VoidMethodCall",
            Mutator::EmptyObjectReturnVals => "EmptyObjectReturnVals",
            Mutator::BooleanFalseReturnVals => "BooleanFalseReturnVals",
            Mutator::BooleanTrueReturnVals => "BooleanTrueReturnVals",
            Mutator::NullReturnVals => "NullReturnVals",
            Mutator::PrimitiveReturns => "PrimitiveReturns",
        }
    }
}

pub type Range = (usize, usize);

/// How a mutant's switch is written into the strong copy: the span it
/// replaces, the parts of it whose own switches are written inside, and the
/// form around them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// The source the switch replaces.
    pub span: Range,
    /// The parts of `span` kept, each with the switches inside it.
    pub holes: Vec<Range>,
    pub form: Form,
    /// Of switches on one span, the outer has the lower layer.
    pub layer: u8,
}

/// The shape of a switch (`jvm_schemata` writes each).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Form {
    /// A condition negated: holes `[condition]`.
    Negate,
    /// A relational comparison with its boundary moved: holes `[left, right]`.
    Compare(&'static str),
    /// A binary arithmetic or bitwise operator replaced: holes `[left,
    /// right]`; the helper is named for the original operator.
    Arith(&'static str),
    /// `x op= y` written twice: holes `[target, value]`.
    Compound {
        from: &'static str,
        to: &'static str,
        statement: bool,
    },
    /// `x++` and the like written twice: holes `[target]`.
    Step {
        from: &'static str,
        to: &'static str,
        prefix: bool,
        statement: bool,
    },
    /// `i += 5` on a local `int`: holes `[target, constant]`.
    StepBy { operator: &'static str },
    /// `-x` with its minus taken away: holes `[operand]`.
    Invert,
    /// A call statement removed: holes `[statement]`. `lambda` where the
    /// call is a lambda's expression body.
    RemoveCall { lambda: bool },
    /// A returned value replaced: holes `[value]`, and the method's declared
    /// return type, where it is written, as the helper's type argument: the
    /// value's own type would add a cast the bytecode mutant does not have.
    Return(ReturnValue, Option<String>),
}

/// What a return is replaced with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnValue {
    True,
    False,
    BoxedTrue,
    BoxedFalse,
    Null,
    /// A primitive zero of this type (`int`, `long`, `float`, `double`,
    /// `byte`, `short`, `char`).
    Zero(Prim),
    /// A boxed zero of this type.
    BoxedZero(Prim),
    EmptyString,
    EmptyList,
    EmptySet,
    EmptyMap,
    EmptyOptional,
    EmptyStream,
}

/// One mutant of a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JvmMutant {
    pub mutator: Mutator,
    pub descriptor: String,
    /// The mutated code: its byte range, and its 1-based line and column.
    pub range: Range,
    pub start: (u32, u32),
    pub end: (u32, u32),
    /// The mutated code as written, on one line.
    pub original: String,
    /// The code with the mutant in place, as a reader is shown it.
    pub replacement: String,
    /// The operator of a binary mutant and where it is in `original`.
    pub operator: Option<(String, usize)>,
    pub placement: Placement,
    /// The method the mutant is in, as PIT names it (`<init>` for a
    /// constructor and for instance initializers), and the class.
    pub method: String,
    pub class: String,
    /// What PIT's description says of the mutant, for joining with its
    /// report: an arithmetic mutant's operation (`+>-`), a removed call's
    /// method name; empty otherwise.
    pub key: String,
    /// The lines PIT may report the mutant on: the compiler puts an
    /// instruction on the line of the statement or call it belongs to, which
    /// need not be the operator's.
    pub lines: (u32, u32),
    /// The last line of the statement around the mutant: kotlinc may put a
    /// multi-line condition's jumps on its last line.
    pub statement_end: u32,
    /// The weak-mutation probe that records what decides whether the
    /// mutant's value differs, where one can be placed without changing
    /// what compiles (`jvm_strong::probe_edits`).
    pub probe: Option<ValueProbe>,
    /// The innermost lambda around the mutant whose body has no coverage
    /// point of its own (an expression body, or a Kotlin lambda a non-inline
    /// function keeps): its code runs when the lambda is called, not when
    /// the statement that makes it runs.
    pub deferred: Option<Range>,
}

/// What a value probe records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProbeKind {
    /// A relational comparison's operands: `lt`, `eq`, `gt`, `unordered`.
    Compare,
    /// An operation and the mutant's beside it: `same`, `differs`.
    Operation,
    /// A returned value: what it is, as the return mutants replace it.
    Return,
}

/// A probe around a mutant's code: insertions only, so it composes with the
/// coverage probes; `\u{2}` stands for its slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueProbe {
    pub kind: ProbeKind,
    pub span: Range,
    /// (offset, rank, text), ranked as `jvm_instrumenter`'s edits are.
    pub edits: Vec<(usize, i32, String)>,
}

/// The rank of a probe's opening at its span's start: inside the coverage
/// probes' wrappers (5, 20), and the shorter span inside the longer.
fn opening_rank(span: Range) -> i32 {
    30 + (1_000_000 - (span.1 - span.0).min(999_999) as i32)
}

/// The rank of a probe's closing at its span's end: outside the coverage
/// wrappers' parentheses (5, 20, 21) that close there, inside the braces
/// (60) and `else` arms of statements that end there.
fn closing_rank_of(_node: Node) -> i32 {
    50
}

/// Whether a Kotlin expression ends in a construct whose branches the
/// coverage instrumenter braces (`if`, `when`, `try`, a lambda): a probe
/// closing there would close inside the last brace.
fn ends_in_braced_branch(node: Node) -> bool {
    let mut current = Some(node);
    while let Some(at) = current {
        if matches!(
            at.kind(),
            "if_expression"
                | "when_expression"
                | "try_expression"
                | "lambda_literal"
                | "annotated_lambda"
                | "anonymous_function"
                | "object_literal"
        ) {
            return true;
        }
        current = (0..at.child_count())
            .rev()
            .filter_map(|index| at.child(index))
            .find(|child| child.end_byte() == node.end_byte() && !child.is_extra());
    }
    false
}

/// The runtime class a probe calls.
const PROBES: &str = "com.supercorp.supercov.Supercov";

// --- Types -----------------------------------------------------------------

/// A primitive type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Prim {
    Boolean,
    Byte,
    Short,
    Char,
    Int,
    Long,
    Float,
    Double,
}

impl Prim {
    pub fn java(self) -> &'static str {
        match self {
            Prim::Boolean => "boolean",
            Prim::Byte => "byte",
            Prim::Short => "short",
            Prim::Char => "char",
            Prim::Int => "int",
            Prim::Long => "long",
            Prim::Float => "float",
            Prim::Double => "double",
        }
    }

    fn from_java(name: &str) -> Option<Prim> {
        Some(match name {
            "boolean" => Prim::Boolean,
            "byte" => Prim::Byte,
            "short" => Prim::Short,
            "char" => Prim::Char,
            "int" => Prim::Int,
            "long" => Prim::Long,
            "float" => Prim::Float,
            "double" => Prim::Double,
            _ => return None,
        })
    }

    fn boxed(self) -> &'static str {
        match self {
            Prim::Boolean => "Boolean",
            Prim::Byte => "Byte",
            Prim::Short => "Short",
            Prim::Char => "Character",
            Prim::Int => "Integer",
            Prim::Long => "Long",
            Prim::Float => "Float",
            Prim::Double => "Double",
        }
    }

    fn from_boxed(name: &str) -> Option<Prim> {
        let name = name.strip_prefix("java.lang.").unwrap_or(name);
        [
            Prim::Boolean,
            Prim::Byte,
            Prim::Short,
            Prim::Char,
            Prim::Int,
            Prim::Long,
            Prim::Float,
            Prim::Double,
        ]
        .into_iter()
        .find(|prim| prim.boxed() == name)
    }

    fn from_kotlin(name: &str) -> Option<Prim> {
        Some(match name {
            "Boolean" => Prim::Boolean,
            "Byte" => Prim::Byte,
            "Short" => Prim::Short,
            "Char" => Prim::Char,
            "Int" => Prim::Int,
            "Long" => Prim::Long,
            "Float" => Prim::Float,
            "Double" => Prim::Double,
            _ => return None,
        })
    }

    fn numeric(self) -> bool {
        !matches!(self, Prim::Boolean)
    }

    /// Binary numeric promotion's rank.
    fn rank(self) -> u8 {
        match self {
            Prim::Boolean => 0,
            Prim::Byte | Prim::Short | Prim::Char | Prim::Int => 1,
            Prim::Long => 2,
            Prim::Float => 3,
            Prim::Double => 4,
        }
    }

    /// The type an arithmetic operation of these two is carried out in.
    fn promote(self, other: Prim) -> Prim {
        let rank = self.rank().max(other.rank());
        match rank {
            4 => Prim::Double,
            3 => Prim::Float,
            2 => Prim::Long,
            _ => Prim::Int,
        }
    }
}

/// What the source says of an expression's type: enough to tell which
/// instruction the compiler makes of it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ty {
    Prim(Prim),
    /// A boxed primitive (`Integer`, Kotlin's `Int?`).
    Boxed(Prim),
    Str,
    /// Any other reference type, by its simple name without arguments
    /// (`List`, `Foo`, `int[]`).
    Object(String),
    Null,
    Void,
    Unknown,
}

impl Ty {
    fn prim(&self) -> Option<Prim> {
        match self {
            Ty::Prim(prim) | Ty::Boxed(prim) => Some(*prim),
            _ => None,
        }
    }

    fn known_reference(&self) -> bool {
        matches!(self, Ty::Str | Ty::Object(_) | Ty::Null)
    }
}

/// A Java type as written (`int`, `Integer`, `List<String>`, `int[]`,
/// `final var`).
fn java_type(text: &str) -> Ty {
    let text = text.trim();
    let base = text.split('<').next().unwrap_or(text).trim();
    if text.ends_with(']') {
        return Ty::Object(text.split_whitespace().collect::<String>());
    }
    let simple = base.rsplit('.').next().unwrap_or(base);
    if let Some(prim) = Prim::from_java(simple) {
        return Ty::Prim(prim);
    }
    if simple == "void" {
        return Ty::Void;
    }
    if simple == "var" {
        return Ty::Unknown;
    }
    if let Some(prim) = Prim::from_boxed(simple) {
        return Ty::Boxed(prim);
    }
    if simple == "String" {
        return Ty::Str;
    }
    Ty::Object(simple.to_owned())
}

/// A Kotlin type as written (`Int`, `Int?`, `List<String>`, `String?`).
fn kotlin_type(text: &str) -> Ty {
    let text = text.trim();
    let nullable = text.ends_with('?');
    let text = text.trim_end_matches('?').trim();
    let text = text
        .strip_prefix('(')
        .and_then(|inner| inner.strip_suffix(')'))
        .unwrap_or(text);
    if text.contains("->") {
        return Ty::Object("Function".into());
    }
    let base = text.split('<').next().unwrap_or(text).trim();
    let simple = base.rsplit('.').next().unwrap_or(base);
    if let Some(prim) = Prim::from_kotlin(simple) {
        return if nullable {
            Ty::Boxed(prim)
        } else {
            Ty::Prim(prim)
        };
    }
    match simple {
        "Unit" | "Nothing" => Ty::Void,
        "String" => Ty::Str,
        _ => Ty::Object(simple.to_owned()),
    }
}

/// The JDK types PIT's `EMPTY_RETURNS` replaces with an empty value, by the
/// simple name of the declared return type: `java.util.List`,
/// `java.util.Collection` and the rest, which a file names without a package.
fn empty_value(ty: &Ty, language: JvmLanguage) -> Option<ReturnValue> {
    match ty {
        Ty::Str => Some(ReturnValue::EmptyString),
        Ty::Boxed(prim) => match prim {
            Prim::Int | Prim::Short | Prim::Char | Prim::Long | Prim::Float | Prim::Double => {
                Some(ReturnValue::BoxedZero(*prim))
            }
            _ => None,
        },
        Ty::Object(name) => Some(match (name.as_str(), language) {
            ("List" | "Collection" | "Iterable", _) => ReturnValue::EmptyList,
            ("MutableList" | "MutableCollection" | "MutableIterable", JvmLanguage::Kotlin) => {
                ReturnValue::EmptyList
            }
            ("Set", _) | ("MutableSet", JvmLanguage::Kotlin) => ReturnValue::EmptySet,
            ("Map", _) | ("MutableMap", JvmLanguage::Kotlin) => ReturnValue::EmptyMap,
            ("Optional", JvmLanguage::Java) => ReturnValue::EmptyOptional,
            ("Stream", JvmLanguage::Java) => ReturnValue::EmptyStream,
            _ => return None,
        }),
        _ => None,
    }
}

impl ReturnValue {
    pub fn descriptor(self) -> &'static str {
        match self {
            ReturnValue::True | ReturnValue::BoxedTrue => "true",
            ReturnValue::False | ReturnValue::BoxedFalse => "false",
            ReturnValue::Null => "null",
            ReturnValue::Zero(_) => "0",
            ReturnValue::BoxedZero(_) => "0",
            ReturnValue::EmptyString => "\"\"",
            ReturnValue::EmptyList => "emptyList",
            ReturnValue::EmptySet => "emptySet",
            ReturnValue::EmptyMap => "emptyMap",
            ReturnValue::EmptyOptional => "Optional.empty",
            ReturnValue::EmptyStream => "Stream.empty",
        }
    }

    fn mutator(self) -> Mutator {
        match self {
            ReturnValue::True | ReturnValue::BoxedTrue => Mutator::BooleanTrueReturnVals,
            ReturnValue::False | ReturnValue::BoxedFalse => Mutator::BooleanFalseReturnVals,
            ReturnValue::Null => Mutator::NullReturnVals,
            ReturnValue::Zero(_) => Mutator::PrimitiveReturns,
            _ => Mutator::EmptyObjectReturnVals,
        }
    }

    /// The value as the reader is shown it.
    fn shown(self, language: JvmLanguage) -> &'static str {
        match (self, language) {
            (ReturnValue::True | ReturnValue::BoxedTrue, _) => "true",
            (ReturnValue::False | ReturnValue::BoxedFalse, _) => "false",
            (ReturnValue::Null, _) => "null",
            (ReturnValue::Zero(Prim::Long), JvmLanguage::Java)
            | (ReturnValue::BoxedZero(Prim::Long), JvmLanguage::Java) => "0L",
            (ReturnValue::Zero(Prim::Long), _) | (ReturnValue::BoxedZero(Prim::Long), _) => "0L",
            (ReturnValue::Zero(Prim::Float), _) | (ReturnValue::BoxedZero(Prim::Float), _) => {
                "0.0f"
            }
            (ReturnValue::Zero(Prim::Double), _) | (ReturnValue::BoxedZero(Prim::Double), _) => {
                "0.0"
            }
            (ReturnValue::Zero(Prim::Char), JvmLanguage::Java)
            | (ReturnValue::BoxedZero(Prim::Char), JvmLanguage::Java) => "'\\0'",
            (ReturnValue::Zero(Prim::Char), _) | (ReturnValue::BoxedZero(Prim::Char), _) => {
                "'\\u0000'"
            }
            (ReturnValue::Zero(_), _) | (ReturnValue::BoxedZero(_), _) => "0",
            (ReturnValue::EmptyString, _) => "\"\"",
            (ReturnValue::EmptyList, JvmLanguage::Java) => "Collections.emptyList()",
            (ReturnValue::EmptySet, JvmLanguage::Java) => "Collections.emptySet()",
            (ReturnValue::EmptyMap, JvmLanguage::Java) => "Collections.emptyMap()",
            (ReturnValue::EmptyList, JvmLanguage::Kotlin) => "emptyList()",
            (ReturnValue::EmptySet, JvmLanguage::Kotlin) => "emptySet()",
            (ReturnValue::EmptyMap, JvmLanguage::Kotlin) => "emptyMap()",
            (ReturnValue::EmptyOptional, _) => "Optional.empty()",
            (ReturnValue::EmptyStream, _) => "Stream.empty()",
        }
    }
}

// --- The project's declarations ------------------------------------------------

/// What the project's own sources declare, read before any file is
/// enumerated: which methods return nothing, by name and number of
/// parameters, so that a call statement is known to call a `void` method.
#[derive(Debug, Clone, Default)]
pub struct ProjectIndex {
    /// Method name to each declaration: its class's simple name, its
    /// parameter count (`None` for a variable count: varargs, Kotlin
    /// defaults), whether it returns nothing (`void`, `Unit`), and its
    /// parameters' types as written.
    methods: HashMap<String, Vec<Declared>>,
    /// Methods' declared return types by name, where every declaration
    /// agrees.
    returns: HashMap<String, Option<Ty>>,
    /// The classes the project declares, by simple name.
    classes: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct Declared {
    class: String,
    arity: Option<usize>,
    void: bool,
    parameters: Vec<String>,
    /// A Kotlin `inline` function: its call is its body, not a call.
    inline: bool,
}

/// JDK types a call's receiver may be declared as, whose methods' results
/// `known_void` knows: a project method of the same name says nothing of
/// them.
fn jdk_receiver(name: &str) -> bool {
    matches!(
        name,
        "List"
            | "ArrayList"
            | "LinkedList"
            | "Set"
            | "HashSet"
            | "LinkedHashSet"
            | "TreeSet"
            | "Map"
            | "HashMap"
            | "LinkedHashMap"
            | "TreeMap"
            | "Collection"
            | "Deque"
            | "ArrayDeque"
            | "Queue"
            | "StringBuilder"
            | "StringBuffer"
            | "Str"
            | "Iterator"
            | "Optional"
            | "Stream"
            | "MutableList"
            | "MutableSet"
            | "MutableMap"
            | "SortedMap"
            | "SortedSet"
            | "NavigableMap"
            | "EnumMap"
            | "Appendable"
            | "Writer"
            | "PrintStream"
            | "PrintWriter"
            | "OutputStream"
            | "InputStream"
            | "Reader"
    )
}

impl ProjectIndex {
    /// The index of these sources (path, text).
    pub fn of<'s>(sources: impl IntoIterator<Item = (&'s str, &'s str)>) -> ProjectIndex {
        let mut index = ProjectIndex::default();
        for (path, source) in sources {
            let language = if path.ends_with(".kt") || path.ends_with(".kts") {
                JvmLanguage::Kotlin
            } else {
                JvmLanguage::Java
            };
            let Ok(tree) = crate::jvm_instrumenter::parse(source, language) else {
                continue;
            };
            index.add(tree.root_node(), source, language, "");
        }
        index
    }

    fn add(&mut self, node: Node, source: &str, language: JvmLanguage, class: &str) {
        let declaration = match (language, node.kind()) {
            (JvmLanguage::Java, "method_declaration") => {
                let name = node
                    .child_by_field_name("name")
                    .map(|name| text(name, source));
                let ty = node
                    .child_by_field_name("type")
                    .map_or(Ty::Unknown, |ty| java_type(text(ty, source)));
                let parameters = node.child_by_field_name("parameters");
                let arity = parameters.map(|parameters| java_arity(parameters));
                let types = parameters
                    .map(|parameters| {
                        named_children(parameters)
                            .filter_map(|parameter| parameter.child_by_field_name("type"))
                            .map(|ty| text(ty, source).to_owned())
                            .collect()
                    })
                    .unwrap_or_default();
                name.map(|name| (name, arity.unwrap_or(Some(0)), ty, types))
            }
            (JvmLanguage::Kotlin, "function_declaration") => {
                let name = named_children(node)
                    .find(|child| child.kind() == "identifier")
                    .map(|name| text(name, source));
                let parameters =
                    named_children(node).find(|child| child.kind() == "function_value_parameters");
                let list = parameters
                    .map(|parameters| {
                        named_children(parameters)
                            .filter(|child| child.kind() == "parameter")
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                // A parameter with a default may be left out, and a
                // `vararg` takes any number.
                let arity = match parameters {
                    Some(parameters)
                        if text(parameters, source).contains('=')
                            || text(parameters, source).contains("vararg") =>
                    {
                        None
                    }
                    _ => Some(list.len()),
                };
                let types = list
                    .iter()
                    .map(|parameter| {
                        named_children(*parameter)
                            .find(|child| child.kind().ends_with("type"))
                            .map(|ty| text(ty, source).to_owned())
                            .unwrap_or_default()
                    })
                    .collect();
                let ty = kotlin_declared_return(node, source);
                name.map(|name| (name, arity, ty, types))
            }
            _ => None,
        };
        let inline = language == JvmLanguage::Kotlin
            && named_children(node)
                .filter(|child| child.kind() == "modifiers")
                .any(|modifiers| {
                    text(modifiers, source)
                        .split_whitespace()
                        .any(|word| word == "inline")
                });
        if let Some((name, arity, ty, parameters)) = declaration {
            let name = name.to_owned();
            self.methods
                .entry(name.clone())
                .or_default()
                .push(Declared {
                    class: class.to_owned(),
                    arity,
                    void: ty == Ty::Void,
                    parameters,
                    inline,
                });
            match self.returns.get(&name) {
                None => {
                    self.returns.insert(name, Some(ty));
                }
                Some(Some(seen)) if *seen != ty => {
                    self.returns.insert(name, None);
                }
                _ => {}
            }
        }
        let inner = match node.kind() {
            "class_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "object_declaration" => {
                let name = node
                    .child_by_field_name("name")
                    .or_else(|| named_children(node).find(|child| child.kind() == "identifier"))
                    .map(|name| text(name, source).to_owned());
                if let Some(name) = &name {
                    self.classes.insert(name.clone());
                }
                name
            }
            _ => None,
        };
        let class = inner.as_deref().unwrap_or(class);
        for child in named_children(node) {
            self.add(child, source, language, class);
        }
    }

    /// Whether a call of `name` with `arguments` arguments, on a receiver of
    /// the type named `receiver` where it is known, returns nothing:
    /// `Some` where the project declares the method, or the JDK's is known.
    fn void_call(&self, name: &str, arguments: usize, receiver: Option<&str>) -> Option<bool> {
        if receiver.is_some_and(jdk_receiver) {
            return known_void(name, arguments);
        }
        if let Some(declarations) = self.methods.get(name) {
            let fitting = declarations
                .iter()
                .filter(|declared| declared.arity.is_none_or(|arity| arity == arguments))
                .collect::<Vec<_>>();
            // The receiver's own class's declarations, where it declares the
            // method; any class's otherwise (an inherited method).
            let own = fitting
                .iter()
                .copied()
                .filter(|declared| receiver.is_some_and(|receiver| declared.class == receiver))
                .collect::<Vec<_>>();
            let matching = if own.is_empty() { fitting } else { own };
            if !matching.is_empty() {
                if matching.iter().all(|declared| declared.void) {
                    return Some(true);
                }
                if matching.iter().all(|declared| !declared.void) {
                    return Some(false);
                }
                return None;
            }
        }
        known_void(name, arguments)
    }

    /// The type a project method of this name declares for its parameter at
    /// `position`, where every declaration of `arguments` parameters agrees.
    fn parameter_type(&self, name: &str, arguments: usize, position: usize) -> Option<&str> {
        let declarations = self.methods.get(name)?;
        let mut found: Option<&str> = None;
        // A lambda goes only where a functional interface is taken: an
        // overload taking a `String` there is not the one called.
        for declared in declarations
            .iter()
            .filter(|declared| declared.arity.is_none_or(|arity| arity == arguments))
        {
            let Some(ty) = declared.parameters.get(position).map(String::as_str) else {
                continue;
            };
            if !names_functional_type(ty) {
                continue;
            }
            match found {
                Some(seen) if seen != ty => return None,
                _ => found = Some(ty),
            }
        }
        found
    }

    /// Whether every project declaration of `name` is a Kotlin `inline`
    /// function.
    fn inline(&self, name: &str) -> bool {
        self.methods
            .get(name)
            .is_some_and(|declarations| declarations.iter().all(|declared| declared.inline))
    }

    fn return_type(&self, name: &str) -> Option<&Ty> {
        self.returns.get(name).and_then(Option::as_ref)
    }
}

fn java_arity(parameters: Node) -> Option<usize> {
    let mut count = 0;
    for child in named_children(parameters) {
        match child.kind() {
            "formal_parameter" => count += 1,
            "spread_parameter" => return None,
            _ => {}
        }
    }
    Some(count)
}

fn kotlin_declared_return(node: Node, source: &str) -> Ty {
    // `fun f(...): T`: the type after the parameters, before the body.
    let mut after_parameters = false;
    for child in children(node) {
        match child.kind() {
            "function_value_parameters" => after_parameters = true,
            "function_body" => {
                // `= expression` with no type written is not `Unit`.
                return if text(child, source).trim_start().starts_with('=') {
                    Ty::Unknown
                } else {
                    Ty::Void
                };
            }
            kind if after_parameters
                && matches!(
                    kind,
                    "user_type" | "nullable_type" | "function_type" | "parenthesized_type"
                ) =>
            {
                return kotlin_type(text(child, source));
            }
            _ => {}
        }
    }
    Ty::Void
}

/// Calls of library methods whose result is known: `Some(true)` for one that
/// returns nothing. A statement calling any other method the project does
/// not declare is taken to keep a result it discards: on the validation
/// projects that guess matched PIT more often than the other.
fn known_void(name: &str, arguments: usize) -> Option<bool> {
    const VOID: &[&str] = &[
        "println",
        "print",
        "clear",
        "sort",
        "forEach",
        "forEachOrdered",
        "close",
        "flush",
        "run",
        "start",
        "join",
        "interrupt",
        "wait",
        "notify",
        "notifyAll",
        "accept",
        "putAll",
        "fill",
        "arraycopy",
        "exit",
        "gc",
        "sleep",
        "ensureCapacity",
        "trimToSize",
        "setLength",
        "lock",
        "unlock",
        "countDown",
        "shutdown",
        "execute",
        "checkArgument",
        "checkState",
        "checkElementIndex",
        "checkPositionIndex",
        "checkPositionIndexes",
        "printStackTrace",
        "addSuppressed",
        "setAccessible",
        "setAll",
        "putBoolean",
        "reset",
        "mark",
        "skipNBytes",
        "addListener",
        "removeListener",
        "invokeLater",
        "require",
        "check",
        "assert",
        "error",
        "TODO",
        "repeat",
        "addFirst",
        "addLast",
        "push",
        "setStackTrace",
        "reverse",
        "shuffle",
        "swap",
        "rotate",
        "initCause",
        "setSeed",
        "nextBytes",
        "assertEquals",
        "assertTrue",
        "assertFalse",
        "assertNull",
        "assertNotNull",
        "fail",
        "set",
        "write",
        "writeBytes",
        "writeInt",
        "writeLong",
        "writeShort",
        "writeByte",
        "writeChar",
        "writeUTF",
        "writeObject",
        "readFully",
        "newLine",
        "rewind",
        "removeAll",
        "retainAll",
    ];
    const VALUED: &[&str] = &[
        "append",
        "add",
        "remove",
        "put",
        "poll",
        "offer",
        "pop",
        "computeIfAbsent",
        "computeIfPresent",
        "compute",
        "merge",
        "putIfAbsent",
        "replace",
        "get",
        "apply",
        "requireNonNull",
        "checkNotNull",
        "format",
        "delete",
        "mkdir",
        "mkdirs",
        "createNewFile",
        "renameTo",
        "compareAndSet",
        "incrementAndGet",
        "decrementAndGet",
        "getAndIncrement",
        "getAndDecrement",
        "getAndSet",
        "getAndAdd",
        "addAndGet",
        "submit",
        "invoke",
        "addAll",
        "insert",
        "reverse",
        "deleteCharAt",
        "toString",
        "equals",
        "hashCode",
        "next",
        "read",
        "skip",
        "contains",
        "isEmpty",
        "size",
        "length",
        "trim",
        "substring",
        "matches",
        "find",
        "lookingAt",
        "take",
        "tryLock",
        "also",
        "let",
        "with",
        "takeIf",
        "takeUnless",
        "mapTo",
        "filterTo",
        "toList",
        "toSet",
        "toMap",
        "plus",
        "minus",
        "copyOf",
        "newInstance",
        "orElse",
        "orElseThrow",
        "orElseGet",
        "build",
        "transferTo",
        "setReadable",
        "setWritable",
        "setExecutable",
        "setLastModified",
        "offerFirst",
        "offerLast",
        "pollFirst",
        "pollLast",
        "removeFirst",
        "removeLast",
        "removeIf",
        "replaceAll",
        "trySplit",
    ];
    if name == "set" && arguments == 2 {
        // `List.set(i, e)` returns the element it replaced.
        return Some(false);
    }
    if name == "add" && arguments == 2 {
        // `List.add(i, e)` returns nothing; `Collection.add(e)` a boolean.
        return Some(true);
    }
    if name == "await" && arguments == 0 {
        return Some(true);
    }
    if VOID.contains(&name) {
        return Some(true);
    }
    if VALUED.contains(&name) {
        return Some(false);
    }
    None
}

// --- Tree helpers ---------------------------------------------------------------

fn text<'s>(node: Node, source: &'s str) -> &'s str {
    &source[node.byte_range()]
}

fn children<'t>(node: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    (0..node.child_count()).filter_map(move |index| node.child(index))
}

fn named_children<'t>(node: Node<'t>) -> impl Iterator<Item = Node<'t>> {
    (0..node.named_child_count()).filter_map(move |index| node.named_child(index))
}

fn one_line(code: &str) -> String {
    code.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 1-based line and column (in characters) of a byte offset.
pub(crate) fn position(source: &str, at: usize) -> (u32, u32) {
    let before = &source[..at];
    let line = before.matches('\n').count() as u32 + 1;
    let line_start = before.rfind('\n').map_or(0, |at| at + 1);
    let column = source[line_start..at].chars().count() as u32 + 1;
    (line, column)
}

fn strip_parens<'t>(mut node: Node<'t>) -> Node<'t> {
    while node.kind() == "parenthesized_expression" {
        match named_children(node).next() {
            Some(inner) => node = inner,
            None => break,
        }
    }
    node
}

// --- Enumeration -----------------------------------------------------------------

/// A variable in scope: its type, whether it is a method's local (a local
/// `int` is incremented with `IINC`, anything else with arithmetic), and
/// whether it is a compile-time constant (the compiler folds arithmetic and
/// comparisons of constants, and PIT never sees them).
#[derive(Debug, Clone)]
struct Var {
    ty: Ty,
    local: bool,
    constant: Option<Constant>,
}

/// A compile-time constant's value, as far as it matters: whether it is zero
/// and whether it is `true`.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Constant {
    Number(f64),
    Bool(bool),
    Text,
}

/// The method a mutant is in.
#[derive(Debug, Clone)]
struct Method {
    /// PIT's name for it.
    name: String,
    /// The declared return type; `None` where it is not known.
    returns: Option<Ty>,
    /// Annotated with something called `NotNull`: PIT leaves its returns
    /// alone for `NULL_RETURNS`.
    not_null: bool,
    /// Code that runs while the class initialises, which PIT does not mutate.
    static_init: bool,
    /// A static field's initializer whose type names a functional
    /// interface: a lambda in it runs later, when called, and PIT mutates it.
    functional_field: bool,
    /// The return type as the declaration writes it, where it does.
    written_returns: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct Class {
    name: String,
    fields: HashMap<String, Var>,
    /// Private methods PIT treats as static initialisation: called from the
    /// class's static initializer, or from such a method.
    initializing: BTreeSet<String>,
    lambdas: usize,
}

/// Enumerates one file's mutants.
pub fn enumerate(
    source: &str,
    tree: &tree_sitter::Tree,
    language: JvmLanguage,
    index: &ProjectIndex,
) -> Vec<JvmMutant> {
    let mut enumerator = Enumerator {
        source,
        language,
        index,
        mutants: Vec::new(),
        scopes: Vec::new(),
        classes: Vec::new(),
        methods: Vec::new(),
        loggers: BTreeSet::new(),
        logging_lines: BTreeSet::new(),
        package: String::new(),
        deferred: Vec::new(),
        root: Some(tree.root_node()),
    };
    enumerator.loggers = logger_names(tree.root_node(), source, language);
    match language {
        JvmLanguage::Java => enumerator.java_node(tree.root_node(), Context::Value),
        JvmLanguage::Kotlin => enumerator.kotlin_file(tree.root_node()),
    }
    let logging = std::mem::take(&mut enumerator.logging_lines);
    let mut mutants = enumerator.mutants;
    // PIT drops every mutant on a line that calls a logging framework.
    mutants.retain(|mutant| !logging.contains(&mutant.start.0));
    mutants.sort_by(|a, b| {
        (
            a.range.0,
            std::cmp::Reverse(a.range.1),
            a.placement.layer,
            a.mutator,
        )
            .cmp(&(
                b.range.0,
                std::cmp::Reverse(b.range.1),
                b.placement.layer,
                b.mutator,
            ))
    });
    mutants.dedup_by(|a, b| {
        a.range == b.range && a.mutator == b.mutator && a.descriptor == b.descriptor
    });
    mutants
}

/// Where an expression is evaluated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Context {
    /// As a value.
    Value,
    /// As a condition the code branches on: a jump.
    Condition,
    /// As a statement, its value discarded.
    Statement,
}

struct Enumerator<'a> {
    source: &'a str,
    language: JvmLanguage,
    index: &'a ProjectIndex,
    mutants: Vec<JvmMutant>,
    scopes: Vec<HashMap<String, Var>>,
    classes: Vec<Class>,
    methods: Vec<Method>,
    /// Names of variables holding a logger from a framework PIT knows.
    loggers: BTreeSet<String>,
    logging_lines: BTreeSet<u32>,
    package: String,
    /// The lambdas being walked whose code runs later (`JvmMutant::deferred`).
    deferred: Vec<Range>,
    /// The file's tree, for the statement around a mutant.
    root: Option<Node<'a>>,
}

/// The logging packages PIT's `LoggingCallsFilter` knows by default.
const LOGGING_PACKAGES: &[&str] = &[
    "java.util.logging",
    "org.apache.log4j",
    "org.apache.logging.log4j",
    "org.slf4j",
    "org.apache.commons.logging",
    "org.jboss.logging",
];

/// Variables and fields that hold a logger of a framework PIT knows: declared
/// with a type imported from one, or initialised by one's factory.
fn logger_names(root: Node, source: &str, language: JvmLanguage) -> BTreeSet<String> {
    let mut imported = BTreeSet::new();
    let mut names = BTreeSet::new();
    fn walk(
        node: Node,
        source: &str,
        language: JvmLanguage,
        imported: &mut BTreeSet<String>,
        names: &mut BTreeSet<String>,
    ) {
        match node.kind() {
            "import_declaration" | "import" => {
                let path = text(node, source)
                    .trim_start_matches("import")
                    .trim()
                    .trim_end_matches(';')
                    .trim()
                    .to_owned();
                if LOGGING_PACKAGES
                    .iter()
                    .any(|package| path.starts_with(&format!("{package}.")))
                {
                    let simple = path.rsplit('.').next().unwrap_or(&path).to_owned();
                    imported.insert(simple);
                }
            }
            "field_declaration" | "local_variable_declaration" if language == JvmLanguage::Java => {
                let ty = node
                    .child_by_field_name("type")
                    .map(|ty| text(ty, source).to_owned())
                    .unwrap_or_default();
                let simple = ty.rsplit('.').next().unwrap_or(&ty).to_owned();
                let qualified = LOGGING_PACKAGES.iter().any(|p| ty.starts_with(p));
                if imported.contains(&simple) || qualified {
                    for declarator in
                        named_children(node).filter(|child| child.kind() == "variable_declarator")
                    {
                        if let Some(name) = declarator.child_by_field_name("name") {
                            names.insert(text(name, source).to_owned());
                        }
                    }
                }
            }
            "property_declaration" if language == JvmLanguage::Kotlin => {
                let declared = text(node, source);
                let logging = imported.iter().any(|name| {
                    declared.contains(&format!(": {name}"))
                        || declared.contains(&format!("{name}."))
                }) || declared.contains("LoggerFactory.getLogger")
                    || declared.contains("LogManager.getLogger");
                if logging
                    && let Some(variable) =
                        named_children(node).find(|child| child.kind() == "variable_declaration")
                    && let Some(name) = named_children(variable).next()
                {
                    names.insert(text(name, source).to_owned());
                }
            }
            _ => {}
        }
        for child in named_children(node) {
            walk(child, source, language, imported, names);
        }
    }
    walk(root, source, language, &mut imported, &mut names);
    names
}

impl<'a> Enumerator<'a> {
    fn method(&self) -> Option<&Method> {
        self.methods.last()
    }

    fn suppressed(&self) -> bool {
        self.method().is_none_or(|method| method.static_init)
    }

    fn lookup(&self, name: &str) -> Option<&Var> {
        for scope in self.scopes.iter().rev() {
            if let Some(var) = scope.get(name) {
                return Some(var);
            }
        }
        for class in self.classes.iter().rev() {
            if let Some(var) = class.fields.get(name) {
                return Some(var);
            }
        }
        None
    }

    fn declare(&mut self, name: &str, var: Var) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), var);
        }
    }

    fn class_name(&self) -> String {
        let nested = self
            .classes
            .iter()
            .map(|class| class.name.as_str())
            .collect::<Vec<_>>()
            .join("$");
        if self.package.is_empty() {
            nested
        } else {
            format!("{}.{nested}", self.package)
        }
    }

    /// Records a mutant of `node` (or of `range` within the file).
    #[allow(clippy::too_many_arguments)]
    fn push(
        &mut self,
        mutator: Mutator,
        descriptor: impl Into<String>,
        range: Range,
        replacement: String,
        operator: Option<(String, usize)>,
        placement: Placement,
        key: String,
        lines: (u32, u32),
    ) {
        if self.suppressed() {
            return;
        }
        let method = self.method().expect("not suppressed").name.clone();
        // PIT's line is the compiler's: kotlinc and javac give an operand on
        // a continuation line its statement's first line.
        let (statement_start, statement_end) = self.statement_lines(range);
        let lines = (lines.0.min(statement_start), lines.1);
        let start = position(self.source, range.0);
        let end = position(self.source, range.1);
        self.mutants.push(JvmMutant {
            mutator,
            descriptor: descriptor.into(),
            range,
            start,
            end,
            original: one_line(&self.source[range.0..range.1]),
            replacement: one_line(&replacement),
            operator,
            placement,
            method,
            class: self.class_name(),
            key,
            lines,
            statement_end,
            probe: None,
            deferred: self.deferred.last().copied(),
        });
    }

    fn line(&self, at: usize) -> u32 {
        position(self.source, at).0
    }

    /// The first line of the statement around `range`: the node whose
    /// parent is a block, a body or a switch group.
    fn statement_lines(&self, range: Range) -> (u32, u32) {
        let Some(root) = self.root else {
            return (self.line(range.0), self.line(range.1));
        };
        let mut node = root.descendant_for_byte_range(range.0, range.1);
        while let Some(at) = node {
            let parent = at.parent();
            if parent.is_none_or(|parent| {
                matches!(
                    parent.kind(),
                    "block"
                        | "statements"
                        | "class_body"
                        | "function_body"
                        | "switch_block"
                        | "switch_block_statement_group"
                        | "switch_rule"
                        | "lambda_literal"
                        | "enum_class_body"
                        | "program"
                        | "source_file"
                )
            }) {
                return (self.line(at.start_byte()), self.line(at.end_byte()));
            }
            node = parent;
        }
        (self.line(range.0), self.line(range.1))
    }

    /// The code of `range` with `[from, to)` replaced by `with`.
    fn spliced(&self, range: Range, from: Range, with: &str) -> String {
        format!(
            "{}{}{}",
            &self.source[range.0..from.0],
            with,
            &self.source[from.1..range.1]
        )
    }
}

/// The helper named for a relational operator.
fn compare_helper(operator: &str) -> Option<(&'static str, &'static str)> {
    Some(match operator {
        "<" => ("lt", "<="),
        "<=" => ("le", "<"),
        ">" => ("gt", ">="),
        ">=" => ("ge", ">"),
        _ => return None,
    })
}

/// PIT's `MATH` replacement of an operator, the helper named for it, and its
/// description's words.
fn math_of(operator: &str) -> Option<(&'static str, &'static str)> {
    Some(match operator {
        "+" => ("-", "add"),
        "-" => ("+", "sub"),
        "*" => ("/", "mul"),
        "/" => ("*", "div"),
        "%" => ("*", "rem"),
        "&" | "and" => ("|", "and"),
        "|" | "or" => ("&", "or"),
        "^" | "xor" => ("&", "xor"),
        "<<" | "shl" => (">>", "shl"),
        ">>" | "shr" => ("<<", "shr"),
        ">>>" | "ushr" => ("<<", "ushr"),
        _ => return None,
    })
}

/// The Kotlin infix function for a Java bitwise operator.
fn kotlin_infix(operator: &str) -> &'static str {
    match operator {
        "|" => "or",
        "&" => "and",
        "<<" => "shl",
        ">>" => "shr",
        _ => "",
    }
}

impl Enumerator<'_> {
    // --- Java -------------------------------------------------------------------

    fn java_children(&mut self, node: Node, context: Context) {
        for child in named_children(node) {
            self.java_node(child, context);
        }
    }

    fn java_node(&mut self, node: Node, context: Context) {
        match node.kind() {
            "package_declaration" => {
                self.package = named_children(node)
                    .find(|child| matches!(child.kind(), "scoped_identifier" | "identifier"))
                    .map(|name| text(name, self.source).to_owned())
                    .unwrap_or_default();
            }
            "import_declaration" | "line_comment" | "block_comment" | "annotation"
            | "marker_annotation" => {}
            "class_declaration"
            | "enum_declaration"
            | "record_declaration"
            | "interface_declaration"
            | "annotation_type_declaration" => self.java_class(node),
            "object_creation_expression" => {
                // An anonymous class is a class of its own, named `Outer$1`.
                for child in named_children(node) {
                    if child.kind() == "class_body" {
                        let position = self.classes.last().map_or(0, |class| class.lambdas) + 1;
                        let class = Class {
                            name: position.to_string(),
                            ..Class::default()
                        };
                        let outer_methods = std::mem::take(&mut self.methods);
                        let static_init = outer_methods.last().is_some_and(|m| m.static_init);
                        self.classes.push(class);
                        self.java_class_body(child, static_init);
                        self.classes.pop();
                        self.methods = outer_methods;
                    } else {
                        self.java_node(child, Context::Value);
                    }
                }
            }
            "method_declaration"
            | "constructor_declaration"
            | "compact_constructor_declaration" => self.java_method(node),
            "lambda_expression" => self.java_lambda(node),
            "local_variable_declaration" => self.java_local(node),
            "block" | "switch_block" | "switch_block_statement_group" | "switch_rule" => {
                self.scopes.push(HashMap::new());
                self.java_children(node, Context::Statement);
                self.scopes.pop();
            }
            "expression_statement" => {
                let Some(expression) = named_children(node).next() else {
                    return;
                };
                self.java_call_statement(node, expression, false);
                self.java_node(expression, Context::Statement);
            }
            "return_statement" => {
                if let Some(value) = named_children(node).next() {
                    // A return from a try-with-resources body is the compiler's
                    // after it closes the resources: PIT's filter leaves it.
                    if !in_resource_try(node) {
                        self.java_return(node, value);
                    }
                    self.java_node(value, Context::Value);
                }
            }
            "if_statement" | "while_statement" | "do_statement" => {
                // javac leaves out what a constant condition never runs.
                let constant = node
                    .child_by_field_name("condition")
                    .and_then(|condition| self.java_constant_value(condition));
                for child in named_children(node) {
                    let dead = match constant {
                        Some(Constant::Bool(false)) => {
                            node.child_by_field_name("consequence") == Some(child)
                                || (node.kind() == "while_statement"
                                    && node.child_by_field_name("body") == Some(child))
                        }
                        Some(Constant::Bool(true)) => {
                            node.child_by_field_name("alternative") == Some(child)
                        }
                        _ => false,
                    };
                    if dead {
                        continue;
                    }
                    let is_condition = node.child_by_field_name("condition") == Some(child);
                    self.java_node(
                        child,
                        if is_condition {
                            Context::Condition
                        } else {
                            Context::Statement
                        },
                    );
                }
                if node.kind() == "while_statement" {
                    self.java_loop_counter(node, None);
                }
            }
            "for_statement" => {
                self.scopes.push(HashMap::new());
                let condition = node.child_by_field_name("condition");
                for child in named_children(node) {
                    let context = if Some(child) == condition {
                        Context::Condition
                    } else {
                        Context::Statement
                    };
                    if node
                        .children_by_field_name("update", &mut node.walk())
                        .any(|update| update == child)
                    {
                        self.java_step(child, true);
                        self.java_children(child, Context::Value);
                    } else {
                        self.java_node(child, context);
                    }
                }
                self.java_loop_counter(node, condition);
                self.scopes.pop();
            }
            "enhanced_for_statement" => {
                self.scopes.push(HashMap::new());
                if let (Some(name), Some(ty)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("type"),
                ) {
                    let ty = java_type(text(ty, self.source));
                    self.declare(
                        text(name, self.source),
                        Var {
                            ty,
                            local: true,
                            constant: None,
                        },
                    );
                }
                for child in named_children(node) {
                    self.java_node(child, Context::Statement);
                }
                self.scopes.pop();
            }
            // PIT's `AssertFilter`: an assertion's code is the compiler's.
            "assert_statement" => {}
            "switch_expression" | "switch_statement" => {
                // A switch on an int, a string or an enum is a table, not a
                // jump PIT negates; only its body is mutated.
                for child in named_children(node) {
                    if child.kind() == "parenthesized_expression" {
                        self.java_children(child, Context::Value);
                    } else {
                        self.java_node(child, Context::Statement);
                    }
                }
            }
            "switch_label" => {}
            "catch_clause" => {
                self.scopes.push(HashMap::new());
                self.java_children(node, Context::Statement);
                self.scopes.pop();
            }
            "try_with_resources_statement" => {
                self.scopes.push(HashMap::new());
                self.java_children(node, Context::Statement);
                self.scopes.pop();
            }
            "parenthesized_expression" => {
                if let Some(inner) = named_children(node).next() {
                    self.java_node(inner, context);
                }
            }
            "binary_expression" => self.java_binary(node, context),
            "unary_expression" => self.java_unary(node, context),
            "update_expression" => {
                self.java_step(node, context == Context::Statement);
                self.java_children(node, Context::Value);
            }
            "assignment_expression" => {
                self.java_assignment(node, context);
            }
            "ternary_expression" => {
                let condition = node.child_by_field_name("condition");
                for child in named_children(node) {
                    let inner = if Some(child) == condition || context == Context::Condition {
                        Context::Condition
                    } else {
                        Context::Value
                    };
                    self.java_node(child, inner);
                }
            }
            "method_invocation" => {
                self.note_logging(node);
                if context == Context::Condition && !self.java_constant(node) {
                    self.negate(node);
                }
                self.java_children(node, Context::Value);
            }
            "instanceof_expression" => {
                // A pattern binds a name whose scope the compiler reads from
                // the condition as written: negating it does not compile.
                if context == Context::Condition {
                    self.negate(node);
                }
                self.java_children(node, Context::Value);
            }
            "identifier" | "field_access" | "array_access" | "cast_expression" => {
                if context == Context::Condition && !self.java_constant(node) {
                    self.negate(node);
                }
                self.java_children(node, Context::Value);
            }
            "true" | "false" => {}
            _ => {
                let inner = if context == Context::Condition {
                    // Anything else in a condition is evaluated as a value
                    // and then jumped on.
                    if self.java_type_of(node) == Ty::Prim(Prim::Boolean)
                        && !self.java_constant(node)
                    {
                        self.negate(node);
                    }
                    Context::Value
                } else {
                    Context::Value
                };
                let inner = if matches!(node.kind(), "program" | "class_body" | "interface_body") {
                    Context::Statement
                } else {
                    inner
                };
                self.java_children(node, inner);
            }
        }
    }

    fn java_class(&mut self, node: Node) {
        let name = node
            .child_by_field_name("name")
            .map(|name| text(name, self.source).to_owned())
            .unwrap_or_default();
        let outer_methods = std::mem::take(&mut self.methods);
        self.classes.push(Class {
            name,
            ..Class::default()
        });
        let enumeration = node.kind() == "enum_declaration";
        for child in named_children(node) {
            match child.kind() {
                "class_body" | "enum_body" | "interface_body" | "annotation_type_body" => {
                    self.java_class_body(child, false);
                }
                "enum_body_declarations" => self.java_class_body(child, false),
                _ => {}
            }
        }
        let _ = enumeration;
        self.classes.pop();
        self.methods = outer_methods;
    }

    /// A class body: its fields first, so that a method above a field knows
    /// its type, then which private methods its static initialisation
    /// calls, then everything in order.
    /// The anonymous classes of code PIT leaves alone (a static
    /// initializer), walked as the classes they are: their methods run when
    /// they are called, not as the class initialises.
    fn java_anonymous_classes_in(&mut self, node: Node) {
        for child in named_children(node) {
            match child.kind() {
                "object_creation_expression" => {
                    for part in named_children(child) {
                        if part.kind() == "class_body" {
                            let position = self.classes.last().map_or(0, |class| class.lambdas) + 1;
                            let outer_methods = std::mem::take(&mut self.methods);
                            self.classes.push(Class {
                                name: position.to_string(),
                                ..Class::default()
                            });
                            self.java_class_body(part, false);
                            self.classes.pop();
                            self.methods = outer_methods;
                        } else {
                            self.java_anonymous_classes_in(part);
                        }
                    }
                }
                "lambda_expression" | "class_declaration" | "class_body" => {}
                _ => self.java_anonymous_classes_in(child),
            }
        }
    }

    fn java_class_body(&mut self, body: Node, static_context: bool) {
        let enumeration = body.kind() == "enum_body"
            || body
                .parent()
                .is_some_and(|parent| parent.kind() == "enum_body");
        let mut members = named_children(body).collect::<Vec<_>>();
        if let Some(declarations) = members
            .iter()
            .find(|member| member.kind() == "enum_body_declarations")
            .copied()
        {
            members.extend(named_children(declarations));
        }
        for member in &members {
            if member.kind() == "field_declaration" || member.kind() == "constant_declaration" {
                let modifiers = modifiers_text(*member, self.source);
                let is_final = modifiers.contains("final")
                    || body.kind() == "interface_body"
                    || member.kind() == "constant_declaration";
                let ty = member
                    .child_by_field_name("type")
                    .map_or(Ty::Unknown, |ty| java_type(text(ty, self.source)));
                for declarator in
                    named_children(*member).filter(|child| child.kind() == "variable_declarator")
                {
                    let Some(name) = declarator.child_by_field_name("name") else {
                        continue;
                    };
                    let constant = if is_final {
                        declarator
                            .child_by_field_name("value")
                            .and_then(|value| self.java_constant_value(value))
                            .filter(|_| matches!(ty, Ty::Prim(_) | Ty::Str))
                    } else {
                        None
                    };
                    let name = text(name, self.source).to_owned();
                    if let Some(class) = self.classes.last_mut() {
                        class.fields.insert(
                            name,
                            Var {
                                ty: ty.clone(),
                                local: false,
                                constant,
                            },
                        );
                    }
                }
            }
        }
        // PIT's `StaticInitializerInterceptor`: private methods the static
        // initializer calls, and those they call, are initialisation. An
        // enum's constructor is called from it too.
        let private = members
            .iter()
            .filter(|member| {
                matches!(
                    member.kind(),
                    "method_declaration" | "constructor_declaration"
                ) && (modifiers_text(**member, self.source).contains("private")
                    || (enumeration && member.kind() == "constructor_declaration"))
            })
            .map(|member| {
                let name = if member.kind() == "constructor_declaration" {
                    "<init>".to_owned()
                } else {
                    member
                        .child_by_field_name("name")
                        .map(|name| text(name, self.source).to_owned())
                        .unwrap_or_default()
                };
                let arity = member
                    .child_by_field_name("parameters")
                    .map_or(Some(0), java_arity);
                (declaration_key(&name, arity), *member)
            })
            .collect::<Vec<_>>();
        let mut initializing = BTreeSet::new();
        let mut pending = Vec::new();
        let class_name = self
            .classes
            .last()
            .map(|class| class.name.clone())
            .unwrap_or_default();
        for member in &members {
            let is_static_code = match member.kind() {
                "static_initializer" => true,
                "field_declaration" => modifiers_text(*member, self.source).contains("static"),
                "enum_constant" => true,
                _ => false,
            };
            if is_static_code {
                collect_calls_in(*member, self.source, &class_name, &mut pending);
                if enumeration && member.kind() == "enum_constant" {
                    let arguments = member
                        .child_by_field_name("arguments")
                        .map_or(0, |arguments| named_children(arguments).count());
                    pending.push(format!("<init>/{arguments}"));
                }
            }
        }
        while let Some(name) = pending.pop() {
            let bodies = private
                .iter()
                .filter(|(key, _)| calls_declaration(&name, key) && !initializing.contains(key))
                .collect::<Vec<_>>();
            for (key, body) in bodies {
                initializing.insert(key.clone());
                collect_calls_in(*body, self.source, &class_name, &mut pending);
            }
        }
        if let Some(class) = self.classes.last_mut() {
            class.initializing = initializing;
        }
        for member in members {
            match member.kind() {
                "field_declaration" | "constant_declaration" => {
                    let is_static = modifiers_text(member, self.source).contains("static")
                        || member.kind() == "constant_declaration"
                        || body.kind() == "interface_body";
                    self.methods.push(Method {
                        name: if is_static { "<clinit>" } else { "<init>" }.to_owned(),
                        returns: None,
                        not_null: false,
                        static_init: is_static || static_context || enumeration,
                        functional_field: is_static
                            && member
                                .child_by_field_name("type")
                                .is_some_and(|ty| names_functional_type(text(ty, self.source))),
                        written_returns: None,
                    });
                    // An enum's instance fields are set in its constructor,
                    // which the static initializer calls.
                    if enumeration
                        && !is_static
                        && !self.classes.last().is_some_and(|class| {
                            class
                                .initializing
                                .iter()
                                .any(|key| key.starts_with("<init>/"))
                        })
                    {
                        self.methods.last_mut().expect("pushed").static_init = false;
                    }
                    self.scopes.push(HashMap::new());
                    for declarator in
                        named_children(member).filter(|child| child.kind() == "variable_declarator")
                    {
                        if let Some(value) = declarator.child_by_field_name("value") {
                            self.java_node(value, Context::Value);
                        }
                    }
                    self.scopes.pop();
                    self.methods.pop();
                }
                "static_initializer" => {
                    // Not mutated by PIT, but an anonymous class declared in
                    // it is a class of its own, whose methods are.
                    self.java_anonymous_classes_in(member);
                }
                "block" => {
                    // An instance initializer runs in every constructor.
                    self.methods.push(Method {
                        name: "<init>".into(),
                        returns: Some(Ty::Void),
                        not_null: false,
                        static_init: static_context,
                        functional_field: false,
                        written_returns: None,
                    });
                    self.java_node(member, Context::Statement);
                    self.methods.pop();
                }
                "enum_constant" => {
                    // Arguments are evaluated by the static initializer; a
                    // body is an anonymous class of its own.
                    if let Some(body) = member.child_by_field_name("body") {
                        let position = self.classes.last().map_or(0, |class| class.lambdas) + 1;
                        self.classes.push(Class {
                            name: position.to_string(),
                            ..Class::default()
                        });
                        self.java_class_body(body, false);
                        self.classes.pop();
                    }
                }
                "enum_body_declarations" => {}
                _ => self.java_node(member, Context::Statement),
            }
        }
    }

    fn java_method(&mut self, node: Node) {
        let constructor = node.kind() != "method_declaration";
        let name = if constructor {
            "<init>".to_owned()
        } else {
            node.child_by_field_name("name")
                .map(|name| text(name, self.source).to_owned())
                .unwrap_or_default()
        };
        let modifiers = modifiers_text(node, self.source);
        let returns = if constructor {
            Ty::Void
        } else {
            node.child_by_field_name("type")
                .map_or(Ty::Unknown, |ty| java_type(text(ty, self.source)))
        };
        // A type variable returns its bound's erasure: `<M extends Map<K, V>>
        // M` returns a `Map` to PIT.
        let returns = match &returns {
            Ty::Object(name) => {
                let bound = named_children(node)
                    .filter(|child| child.kind() == "type_parameters")
                    .flat_map(named_children)
                    .find(|parameter| {
                        named_children(*parameter)
                            .next()
                            .is_some_and(|first| text(first, self.source) == name)
                    })
                    .map(|parameter| {
                        named_children(parameter)
                            .find(|child| child.kind() == "type_bound")
                            .and_then(|bound| named_children(bound).next())
                            .map_or(Ty::Object("Object".into()), |bound| {
                                java_type(text(bound, self.source))
                            })
                    });
                bound.unwrap_or(returns)
            }
            _ => returns,
        };
        let key = declaration_key(
            &name,
            node.child_by_field_name("parameters")
                .map_or(Some(0), java_arity),
        );
        let initializing = self
            .classes
            .last()
            .is_some_and(|class| class.initializing.contains(&key));
        let generated = modifiers.contains("@Generated")
            || modifiers.contains("@DoNotMutate")
            || modifiers.contains("@CoverageIgnore");
        self.methods.push(Method {
            name,
            returns: Some(returns),
            not_null: modifiers
                .split(|c: char| !c.is_alphanumeric() && c != '@')
                .any(|word| word == "@NotNull")
                || modifiers.contains(".NotNull"),
            static_init: initializing || generated,
            functional_field: false,
            written_returns: (!constructor)
                .then(|| node.child_by_field_name("type"))
                .flatten()
                .map(|ty| without_annotations(text(ty, self.source))),
        });
        self.scopes.push(HashMap::new());
        if let Some(parameters) = node.child_by_field_name("parameters") {
            self.java_parameters(parameters);
        }
        if let Some(body) = node.child_by_field_name("body") {
            self.java_node(body, Context::Statement);
        }
        self.scopes.pop();
        self.methods.pop();
    }

    fn java_parameters(&mut self, parameters: Node) {
        for parameter in named_children(parameters) {
            if !matches!(parameter.kind(), "formal_parameter" | "spread_parameter") {
                if parameter.kind() == "identifier" {
                    self.declare(
                        text(parameter, self.source),
                        Var {
                            ty: Ty::Unknown,
                            local: true,
                            constant: None,
                        },
                    );
                }
                continue;
            }
            let ty = parameter
                .child_by_field_name("type")
                .map_or(Ty::Unknown, |ty| java_type(text(ty, self.source)));
            let name = parameter.child_by_field_name("name").or_else(|| {
                named_children(parameter)
                    .filter(|child| child.kind() == "variable_declarator")
                    .find_map(|declarator| declarator.child_by_field_name("name"))
            });
            if let Some(name) = name {
                let ty = if parameter.kind() == "spread_parameter" {
                    Ty::Object("[]".into())
                } else {
                    ty
                };
                self.declare(
                    text(name, self.source),
                    Var {
                        ty,
                        local: true,
                        constant: None,
                    },
                );
            }
        }
    }

    fn java_lambda(&mut self, node: Node) {
        let enclosing = self
            .method()
            .map(|method| method.name.clone())
            .unwrap_or_default();
        // A lambda a static field of a functional type keeps runs when it is
        // called, not as the class initialises: PIT mutates it.
        let functional = self.method().is_some_and(|method| method.functional_field);
        let static_init = self.suppressed() && !functional;
        let enclosing = match enclosing.as_str() {
            "<init>" => "new".to_owned(),
            "<clinit>" => "static".to_owned(),
            other => other.to_owned(),
        };
        let returns = self.java_lambda_returns(node);
        self.methods.push(Method {
            name: format!("lambda${enclosing}$"),
            returns,
            not_null: false,
            static_init,
            functional_field: false,
            written_returns: None,
        });
        self.scopes.push(HashMap::new());
        if let Some(parameters) = node.child_by_field_name("parameters") {
            match parameters.kind() {
                "identifier" => self.declare(
                    text(parameters, self.source),
                    Var {
                        ty: Ty::Unknown,
                        local: true,
                        constant: None,
                    },
                ),
                _ => self.java_parameters(parameters),
            }
        }
        if let Some(body) = node.child_by_field_name("body") {
            if body.kind() == "block" {
                self.java_node(body, Context::Statement);
            } else {
                self.deferred.push((node.start_byte(), node.end_byte()));
                // An expression body is the lambda's return value, or, in a
                // lambda that returns nothing, its statement.
                let returns = self.method().and_then(|method| method.returns.clone());
                if returns == Some(Ty::Void) {
                    self.java_call_statement(body, body, true);
                    self.java_node(body, Context::Statement);
                } else {
                    self.java_return(body, body);
                    self.java_node(body, Context::Value);
                }
                self.deferred.pop();
            }
        }
        self.scopes.pop();
        self.methods.pop();
    }

    /// What a lambda returns, from the method it is passed to or the type it
    /// is assigned to: what javac declares its synthetic method to return.
    fn java_lambda_returns(&self, node: Node) -> Option<Ty> {
        let parent = node.parent()?;
        let body_ty = || {
            node.child_by_field_name("body")
                .filter(|body| body.kind() != "block")
                .map(|body| self.java_type_of(body))
        };
        match parent.kind() {
            "argument_list" => {
                let call = parent.parent()?;
                let name = call
                    .child_by_field_name("name")
                    .map(|name| text(name, self.source))?;
                // A project method says what it takes; the JDK's are known.
                let arguments = named_children(parent).collect::<Vec<_>>();
                let position = arguments.iter().position(|argument| *argument == node)?;
                match self.index.parameter_type(name, arguments.len(), position) {
                    Some(declared) => functional_type_return(declared, body_ty),
                    None => functional_return(name, body_ty),
                }
            }
            "variable_declarator" => {
                let declaration = parent.parent()?;
                let ty = declaration.child_by_field_name("type")?;
                functional_type_return(text(ty, self.source), body_ty)
            }
            _ => None,
        }
    }

    fn java_local(&mut self, node: Node) {
        let declared = node
            .child_by_field_name("type")
            .map_or(Ty::Unknown, |ty| java_type(text(ty, self.source)));
        let is_final = modifiers_text(node, self.source).contains("final");
        for child in named_children(node) {
            if child.kind() != "variable_declarator" {
                self.java_node(child, Context::Value);
                continue;
            }
            let value = child.child_by_field_name("value");
            if let Some(value) = value {
                self.java_node(value, Context::Value);
            }
            let Some(name) = child.child_by_field_name("name") else {
                continue;
            };
            let ty = if declared == Ty::Unknown {
                value.map_or(Ty::Unknown, |value| self.java_type_of(value))
            } else {
                declared.clone()
            };
            let constant = if is_final && matches!(ty, Ty::Prim(_) | Ty::Str) {
                value.and_then(|value| self.java_constant_value(value))
            } else {
                None
            };
            let ty = if child.child_by_field_name("dimensions").is_some() {
                Ty::Object("[]".into())
            } else {
                ty
            };
            self.declare(
                text(name, self.source),
                Var {
                    ty,
                    local: true,
                    constant,
                },
            );
        }
    }

    /// A call statement: PIT's `VOID_METHOD_CALLS` removes a call to a method
    /// that returns nothing. A constructor's `super(...)` is not one.
    fn java_call_statement(&mut self, statement: Node, expression: Node, lambda: bool) {
        if expression.kind() != "method_invocation" {
            return;
        }
        let Some(name) = expression
            .child_by_field_name("name")
            .map(|name| text(name, self.source).to_owned())
        else {
            return;
        };
        let arguments = expression
            .child_by_field_name("arguments")
            .map_or(0, |arguments| named_children(arguments).count());
        let receiver = match expression.child_by_field_name("object") {
            None => self.classes.last().map(|class| class.name.clone()),
            Some(object) if text(object, self.source) == "this" => {
                self.classes.last().map(|class| class.name.clone())
            }
            Some(object) => match self.java_type_of(object) {
                Ty::Object(name) => Some(name),
                Ty::Str => Some("Str".to_owned()),
                _ => {
                    // A class's own name: a static call.
                    let named = text(object, self.source);
                    self.index.classes.contains(named).then(|| named.to_owned())
                }
            },
        };
        if self.index.void_call(&name, arguments, receiver.as_deref()) != Some(true) {
            return;
        }
        let range = (statement.start_byte(), statement.end_byte());
        let lines = (self.line(range.0), self.line(range.1));
        self.push(
            Mutator::VoidMethodCall,
            "removed",
            range,
            String::new(),
            None,
            Placement {
                span: range,
                holes: vec![range],
                form: Form::RemoveCall { lambda },
                layer: 0,
            },
            name,
            lines,
        );
    }

    fn note_logging(&mut self, call: Node) {
        let Some(object) = call.child_by_field_name("object") else {
            return;
        };
        let receiver = text(object, self.source);
        let simple = receiver.rsplit('.').next().unwrap_or(receiver);
        if self.loggers.contains(simple)
            || matches!(receiver, "LoggerFactory" | "LogManager" | "LogFactory")
        {
            let line = call
                .child_by_field_name("name")
                .map_or(self.line(call.start_byte()), |name| {
                    self.line(name.start_byte())
                });
            self.logging_lines.insert(line);
        }
    }

    /// A value returned from the method in `self.method()`: the return
    /// mutants PIT makes for its type.
    fn java_return(&mut self, statement: Node, value: Node) {
        let Some(method) = self.method() else {
            return;
        };
        let Some(returns) = method.returns.clone() else {
            return;
        };
        let not_null = method.not_null;
        let value = strip_parens(value);
        let tail = self.java_tail(value);
        let range = (value.start_byte(), value.end_byte());
        let lines = (
            self.line(statement.start_byte()),
            self.line(statement.end_byte()),
        );
        let mut replacements = Vec::new();
        match &returns {
            Ty::Void | Ty::Unknown => {}
            Ty::Prim(Prim::Boolean) => {
                if tail != Tail::One {
                    replacements.push(ReturnValue::True);
                }
                if tail != Tail::Zero {
                    replacements.push(ReturnValue::False);
                }
            }
            Ty::Boxed(Prim::Boolean) => {
                if tail != Tail::One {
                    replacements.push(ReturnValue::BoxedTrue);
                }
                if tail != Tail::Zero {
                    replacements.push(ReturnValue::BoxedFalse);
                }
            }
            Ty::Prim(prim) => {
                if tail != Tail::Zero {
                    replacements.push(ReturnValue::Zero(*prim));
                }
            }
            other => {
                if let Some(empty) = empty_value(other, self.language) {
                    if tail != Tail::Empty
                        && !(matches!(empty, ReturnValue::BoxedZero(_)) && tail == Tail::Zero)
                    {
                        replacements.push(empty);
                    }
                } else if tail != Tail::Null && !not_null {
                    replacements.push(ReturnValue::Null);
                }
            }
        }
        let witness = method.written_returns.clone();
        let method_name = method.name.clone();
        let cast = match (&returns, self.language) {
            (Ty::Prim(prim @ (Prim::Byte | Prim::Short | Prim::Char)), JvmLanguage::Java) => {
                Some(prim.java())
            }
            _ => None,
        };
        let probe = return_probe(value, cast);
        let before = self.mutants.len();
        for value_replacement in replacements {
            self.push(
                value_replacement.mutator(),
                value_replacement.descriptor(),
                range,
                value_replacement.shown(self.language).to_owned(),
                None,
                Placement {
                    span: range,
                    holes: vec![range],
                    form: Form::Return(value_replacement, witness.clone()),
                    layer: 0,
                },
                method_name.clone(),
                lines,
            );
        }
        // One probe serves every return mutant of the value.
        let placeable = self.language == JvmLanguage::Java || !ends_in_braced_branch(value);
        if placeable
            && self.mutants.len() > before
            && let Some(first) = self.mutants.get_mut(before)
        {
            first.probe = Some(probe);
        }
    }

    /// The constant javac leaves on the stack last for a returned
    /// expression, where it is one PIT's equivalence filter reads: `return
    /// a < b` ends `iconst_0`, as `return false` does.
    fn java_tail(&self, value: Node) -> Tail {
        let value = strip_parens(value);
        match value.kind() {
            "ternary_expression" => value
                .child_by_field_name("alternative")
                .map_or(Tail::Other, |alternative| self.java_tail(alternative)),
            "binary_expression" => {
                let operator = binary_operator(value, self.source);
                if matches!(
                    operator.as_str(),
                    "<" | "<=" | ">" | ">=" | "==" | "!=" | "&&" | "||"
                ) {
                    if self.java_constant(value) {
                        return match self.java_constant_value(value) {
                            Some(Constant::Bool(true)) => Tail::One,
                            _ => Tail::Zero,
                        };
                    }
                    return Tail::Zero;
                }
                self.constant_tail(value)
            }
            "unary_expression"
                if value
                    .child_by_field_name("operator")
                    .is_some_and(|op| text(op, self.source) == "!") =>
            {
                if self.java_constant(value) {
                    return self.constant_tail(value);
                }
                Tail::Zero
            }
            "null_literal" => Tail::Null,
            "field_access" if one_line(text(value, self.source)) == "Boolean.FALSE" => Tail::Zero,
            "field_access" if one_line(text(value, self.source)) == "Boolean.TRUE" => Tail::One,
            "string_literal" if text(value, self.source) == "\"\"" => Tail::Empty,
            "method_invocation" => {
                let call = text(value, self.source)
                    .split_whitespace()
                    .collect::<String>();
                if [
                    "Collections.emptyList()",
                    "Collections.emptySet()",
                    "Collections.emptyMap()",
                    "Optional.empty()",
                    "Stream.empty()",
                    "List.of()",
                    "Set.of()",
                    "Map.of()",
                ]
                .iter()
                .any(|empty| call.ends_with(empty))
                {
                    Tail::Empty
                } else {
                    Tail::Other
                }
            }
            _ => self.constant_tail(value),
        }
    }

    fn constant_tail(&self, value: Node) -> Tail {
        match self.java_constant_value(value) {
            Some(Constant::Number(0.0)) => Tail::Zero,
            Some(Constant::Number(1.0)) => Tail::One,
            Some(Constant::Bool(false)) => Tail::Zero,
            Some(Constant::Bool(true)) => Tail::One,
            _ => Tail::Other,
        }
    }

    fn java_binary(&mut self, node: Node, context: Context) {
        let operator = binary_operator(node, self.source);
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        let (Some(left), Some(right)) = (left, right) else {
            self.java_children(node, Context::Value);
            return;
        };
        match operator.as_str() {
            "&&" | "||" => {
                // Short-circuit operators are jumps on their operands, as a
                // condition or as a value.
                self.java_node(left, Context::Condition);
                self.java_node(right, Context::Condition);
            }
            "<" | "<=" | ">" | ">=" | "==" | "!=" => {
                if !self.java_constant(node) {
                    if let Some((helper, to)) = compare_helper(&operator) {
                        self.boundary(node, left, right, &operator, helper, to);
                    }
                    self.negate(node);
                }
                self.java_node(left, Context::Value);
                self.java_node(right, Context::Value);
            }
            _ => {
                if let Some((to, helper)) = math_of(&operator)
                    && !self.java_constant(node)
                    && !self.java_concatenates(node, &operator)
                {
                    self.arith(node, left, right, &operator, to, helper);
                }
                if context == Context::Condition
                    && matches!(operator.as_str(), "&" | "|" | "^")
                    && !self.java_constant(node)
                {
                    // A non-short-circuit operator is a value jumped on.
                    self.negate(node);
                }
                self.java_node(left, Context::Value);
                self.java_node(right, Context::Value);
            }
        }
    }

    /// Whether `+` here is string concatenation, which javac compiles to no
    /// arithmetic at all.
    fn java_concatenates(&self, node: Node, operator: &str) -> bool {
        if operator != "+" {
            return false;
        }
        let ty = self.java_type_of(node);
        ty == Ty::Str
    }

    fn java_unary(&mut self, node: Node, context: Context) {
        let operator = node
            .child_by_field_name("operator")
            .map(|op| text(op, self.source).to_owned())
            .unwrap_or_default();
        let Some(operand) = node.child_by_field_name("operand") else {
            return;
        };
        match operator.as_str() {
            "!" => {
                // `!x` is a jump on `x`, inverted: one jump, PIT's to negate.
                self.java_node(operand, Context::Condition);
            }
            "-" => {
                if !self.java_constant(node) {
                    self.invert(node, operand);
                }
                if context == Context::Condition {
                    self.negate(node);
                }
                self.java_node(operand, Context::Value);
            }
            _ => self.java_node(operand, Context::Value),
        }
    }

    fn java_assignment(&mut self, node: Node, context: Context) {
        let operator = node
            .child_by_field_name("operator")
            .map(|op| text(op, self.source).to_owned())
            .unwrap_or_default();
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            return;
        };
        if operator != "=" {
            self.java_compound(node, left, right, &operator, context);
        }
        if context == Context::Condition {
            self.negate(node);
        }
        self.java_node(left, Context::Value);
        self.java_node(right, Context::Value);
    }

    /// `x op= y`: an `IINC` on a local `int` by a constant (Increments),
    /// arithmetic otherwise (Math).
    fn java_compound(
        &mut self,
        node: Node,
        left: Node,
        right: Node,
        operator: &str,
        context: Context,
    ) {
        let base = operator.trim_end_matches('=');
        let target = self.java_type_of(left);
        if base == "+" && (target == Ty::Str || target.known_reference() && target.prim().is_none())
        {
            return;
        }
        let local_int = self.local_int(left);
        let step = match self.language {
            JvmLanguage::Java => self.java_constant_value(right),
            JvmLanguage::Kotlin => self.kotlin_constant_value(right),
        };
        let range = (node.start_byte(), node.end_byte());
        let operator_range = node
            .child_by_field_name("operator")
            .map(|op| (op.start_byte(), op.end_byte()))
            .unwrap_or((left.end_byte(), right.start_byte()));
        let lines = (self.line(range.0), self.line(range.1));
        if local_int
            && matches!(base, "+" | "-")
            && let Some(Constant::Number(value)) = step
            && value.fract() == 0.0
            && (-32768.0..=32767.0).contains(&if base == "-" { -value } else { value })
        {
            let to = if base == "+" { "-=" } else { "+=" };
            let delta = if base == "+" { value } else { -value };
            let replacement = self.spliced(range, operator_range, to);
            let statement = context == Context::Statement;
            self.push(
                Mutator::Increments,
                to,
                range,
                replacement,
                Some((operator.to_owned(), operator_range.0 - range.0)),
                Placement {
                    span: range,
                    holes: vec![
                        (left.start_byte(), left.end_byte()),
                        (right.start_byte(), right.end_byte()),
                    ],
                    form: if statement {
                        Form::StepBy {
                            operator: if base == "+" { "+=" } else { "-=" },
                        }
                    } else {
                        Form::Compound {
                            from: if base == "+" { "+=" } else { "-=" },
                            to,
                            statement: false,
                        }
                    },
                    layer: 2,
                },
                format!("{}", delta as i64),
                lines,
            );
            return;
        }
        let Some((to, _)) = math_of(base) else {
            return;
        };
        let to_operator: &'static str = match to {
            "-" => "-=",
            "+" => "+=",
            "/" => "/=",
            "*" => "*=",
            "|" => "|=",
            "&" => "&=",
            ">>" => ">>=",
            "<<" => "<<=",
            _ => return,
        };
        let from_operator: &'static str = match operator {
            "+=" => "+=",
            "-=" => "-=",
            "*=" => "*=",
            "/=" => "/=",
            "%=" => "%=",
            "&=" => "&=",
            "|=" => "|=",
            "^=" => "^=",
            "<<=" => "<<=",
            ">>=" => ">>=",
            ">>>=" => ">>>=",
            _ => return,
        };
        let replacement = self.spliced(range, operator_range, to_operator);
        self.push(
            Mutator::Math,
            to_operator,
            range,
            replacement,
            Some((operator.to_owned(), operator_range.0 - range.0)),
            Placement {
                span: range,
                holes: vec![
                    (left.start_byte(), left.end_byte()),
                    (right.start_byte(), right.end_byte()),
                ],
                form: Form::Compound {
                    from: from_operator,
                    to: to_operator,
                    statement: context == Context::Statement,
                },
                layer: 2,
            },
            format!("{base}>{to}"),
            lines,
        );
    }

    /// Whether `node` names a local `int`: what javac increments with `IINC`.
    fn local_int(&self, node: Node) -> bool {
        let node = strip_parens(node);
        node.kind() == "identifier"
            && self
                .lookup(text(node, self.source))
                .is_some_and(|var| var.local && var.ty == Ty::Prim(Prim::Int))
    }

    /// `x++`, `--x` and the like: Increments on a local `int`, Math anywhere
    /// else. `in_update` for a `for` loop's update, a statement.
    fn java_step(&mut self, node: Node, in_update: bool) {
        if node.kind() != "update_expression" {
            if node.kind() == "assignment_expression" && in_update {
                // A compound assignment in a `for` update is walked with
                // the others; it is a statement.
                let operator = node
                    .child_by_field_name("operator")
                    .map(|op| text(op, self.source).to_owned())
                    .unwrap_or_default();
                if let (Some(left), Some(right)) = (
                    node.child_by_field_name("left"),
                    node.child_by_field_name("right"),
                ) && operator != "="
                {
                    self.java_compound(node, left, right, &operator, Context::Statement);
                }
            }
            return;
        }
        let text_of = text(node, self.source);
        let prefix = text_of.starts_with("++") || text_of.starts_with("--");
        let increment = text_of.contains("++");
        let Some(target) = named_children(node).next() else {
            return;
        };
        self.step(node, target, prefix, increment, in_update);
    }

    fn step(&mut self, node: Node, target: Node, prefix: bool, increment: bool, statement: bool) {
        let range = (node.start_byte(), node.end_byte());
        let local_int = self.local_int(target);
        let (from, to): (&'static str, &'static str) = if increment {
            ("++", "--")
        } else {
            ("--", "++")
        };
        let operator_at = if prefix {
            0
        } else {
            target.end_byte() - range.0
        };
        let replacement = if prefix {
            format!("{to}{}", text(target, self.source))
        } else {
            format!("{}{to}", text(target, self.source))
        };
        let lines = (self.line(range.0), self.line(range.1));
        let (mutator, key) = if local_int {
            (
                Mutator::Increments,
                if increment { "1" } else { "-1" }.to_owned(),
            )
        } else {
            (
                Mutator::Math,
                if increment { "+>-" } else { "->+" }.to_owned(),
            )
        };
        self.push(
            mutator,
            to,
            range,
            replacement,
            Some((from.to_owned(), operator_at)),
            Placement {
                span: range,
                holes: vec![(target.start_byte(), target.end_byte())],
                form: Form::Step {
                    from,
                    to,
                    prefix,
                    statement,
                },
                layer: 2,
            },
            key,
            lines,
        );
    }

    /// PIT's `AvoidForLoopCounterFilter`: a loop whose condition starts by
    /// reading a local and whose last update (or body statement, for a
    /// `while`) increments it with `IINC` keeps that increment unmutated.
    fn java_loop_counter(&mut self, node: Node, condition: Option<Node>) {
        let condition = condition.or_else(|| node.child_by_field_name("condition"));
        let Some(condition) = condition else {
            return;
        };
        let Some(counter) = first_read(condition, self.source) else {
            return;
        };
        let last = if node.kind() == "for_statement" {
            node.children_by_field_name("update", &mut node.walk())
                .last()
        } else {
            node.child_by_field_name("body").and_then(|body| {
                if body.kind() == "block" {
                    named_children(body)
                        .filter(|child| !child.kind().ends_with("comment"))
                        .last()
                        .and_then(|statement| {
                            (statement.kind() == "expression_statement")
                                .then(|| named_children(statement).next())
                                .flatten()
                        })
                } else if body.kind() == "expression_statement" {
                    named_children(body).next()
                } else {
                    None
                }
            })
        };
        let Some(last) = last else {
            return;
        };
        let target = match last.kind() {
            "update_expression" => named_children(last).next(),
            "assignment_expression" => last.child_by_field_name("left"),
            _ => None,
        };
        if target.is_some_and(|target| text(target, self.source) == counter) {
            let range = (last.start_byte(), last.end_byte());
            self.mutants
                .retain(|mutant| !(mutant.mutator == Mutator::Increments && mutant.range == range));
        }
    }

    // --- Mutants -----------------------------------------------------------------

    fn negate(&mut self, node: Node) {
        let node = strip_parens(node);
        let range = (node.start_byte(), node.end_byte());
        let lines = (self.line(range.0), self.line(range.1));
        let replacement = format!("!({})", &self.source[range.0..range.1]);
        self.push(
            Mutator::NegateConditionals,
            "negated",
            range,
            replacement,
            None,
            Placement {
                span: range,
                holes: vec![range],
                form: Form::Negate,
                layer: 1,
            },
            String::new(),
            lines,
        );
    }

    fn boundary(
        &mut self,
        node: Node,
        left: Node,
        right: Node,
        operator: &str,
        helper: &'static str,
        to: &'static str,
    ) {
        let range = (node.start_byte(), node.end_byte());
        let (operator_range, _) = operator_range(node, left, right, self.source);
        let replacement = self.spliced(range, operator_range, to);
        let lines = (self.line(range.0), self.line(range.1));
        let before = self.mutants.len();
        let probe = self.binary_probe_safe(left, right, operator).then(|| {
            binary_probe(
                ProbeKind::Compare,
                &format!("w{helper}"),
                range,
                left,
                operator_range,
                right,
            )
        });
        self.push(
            Mutator::ConditionalsBoundary,
            to,
            range,
            replacement,
            Some((operator.to_owned(), operator_range.0 - range.0)),
            Placement {
                span: range,
                holes: vec![
                    (left.start_byte(), left.end_byte()),
                    (right.start_byte(), right.end_byte()),
                ],
                form: Form::Compare(helper),
                layer: 2,
            },
            String::new(),
            lines,
        );
        self.attach(before, probe);
    }

    fn arith(
        &mut self,
        node: Node,
        left: Node,
        right: Node,
        operator: &str,
        to: &'static str,
        helper: &'static str,
    ) {
        let range = (node.start_byte(), node.end_byte());
        let (operator_range, _) = operator_range(node, left, right, self.source);
        let written_to = if self.language == JvmLanguage::Kotlin
            && !kotlin_infix(to).is_empty()
            && operator.chars().all(char::is_alphabetic)
        {
            kotlin_infix(to)
        } else {
            to
        };
        let replacement = self.spliced(range, operator_range, written_to);
        let lines = (self.line(range.0), self.line(range.1));
        let before = self.mutants.len();
        let probe = self.binary_probe_safe(left, right, operator).then(|| {
            binary_probe(
                ProbeKind::Operation,
                &format!("w{helper}"),
                range,
                left,
                operator_range,
                right,
            )
        });
        let from = match operator {
            "and" => "&",
            "or" => "|",
            "xor" => "^",
            "shl" => "<<",
            "shr" => ">>",
            "ushr" => ">>>",
            other => other,
        };
        self.push(
            Mutator::Math,
            written_to,
            range,
            replacement,
            Some((operator.to_owned(), operator_range.0 - range.0)),
            Placement {
                span: range,
                holes: vec![
                    (left.start_byte(), left.end_byte()),
                    (right.start_byte(), right.end_byte()),
                ],
                form: Form::Arith(helper),
                layer: 2,
            },
            format!("{from}>{to}"),
            lines,
        );
        self.attach(before, probe);
    }

    /// Gives the mutant `push` just added its probe.
    fn attach(&mut self, before: usize, probe: Option<ValueProbe>) {
        if self.mutants.len() > before
            && let Some(mutant) = self.mutants.last_mut()
        {
            mutant.probe = probe;
        }
    }

    /// Whether a probe call in place of this operator compiles exactly
    /// where the operator did: Java's operators other than `+` take only
    /// numbers (and booleans, which the helpers take too); a `+` and every
    /// Kotlin operator only where both operands' types are known primitives
    /// the helpers take, the same type in Kotlin, which does not widen.
    fn binary_probe_safe(&self, left: Node, right: Node, operator: &str) -> bool {
        match self.language {
            JvmLanguage::Java => {
                operator != "+"
                    || [left, right].iter().all(|side| {
                        self.java_type_of(*side)
                            .prim()
                            .is_some_and(|prim| prim.numeric())
                    })
            }
            JvmLanguage::Kotlin => {
                if ends_in_braced_branch(right) {
                    return false;
                }
                let (Ty::Prim(a), Ty::Prim(b)) =
                    (self.kotlin_type_of(left), self.kotlin_type_of(right))
                else {
                    return false;
                };
                let shift = matches!(operator, "shl" | "shr" | "ushr");
                let logical = matches!(operator, "and" | "or" | "xor");
                let wide = matches!(a, Prim::Int | Prim::Long | Prim::Float | Prim::Double);
                if shift {
                    matches!(a, Prim::Int | Prim::Long) && b == Prim::Int
                } else if logical {
                    a == b && matches!(a, Prim::Int | Prim::Long | Prim::Boolean)
                } else {
                    a == b && wide
                }
            }
        }
    }

    fn invert(&mut self, node: Node, operand: Node) {
        let range = (node.start_byte(), node.end_byte());
        let lines = (self.line(range.0), self.line(range.1));
        let before = self.mutants.len();
        let safe = match self.language {
            JvmLanguage::Java => true,
            JvmLanguage::Kotlin => {
                !ends_in_braced_branch(operand)
                    && matches!(
                        self.kotlin_type_of(operand),
                        Ty::Prim(Prim::Int | Prim::Long | Prim::Float | Prim::Double)
                    )
            }
        };
        let minus = (range.0, operand.start_byte());
        let probe = safe.then(|| ValueProbe {
            kind: ProbeKind::Operation,
            span: range,
            edits: vec![
                (
                    minus.0,
                    opening_rank(range),
                    format!("{PROBES}.wneg(\u{2}, /*"),
                ),
                (minus.1, 10, "*/".to_owned()),
                (operand.end_byte(), closing_rank_of(node), ")".to_owned()),
            ],
        });
        self.push(
            Mutator::InvertNegs,
            "removed",
            range,
            text(operand, self.source).to_owned(),
            Some(("-".to_owned(), 0)),
            Placement {
                span: range,
                holes: vec![(operand.start_byte(), operand.end_byte())],
                form: Form::Invert,
                layer: 2,
            },
            String::new(),
            lines,
        );
        self.attach(before, probe);
    }
}

/// Whether a statement is in the body of a try-with-resources.
fn in_resource_try(node: Node) -> bool {
    let mut current = node;
    while let Some(parent) = current.parent() {
        match parent.kind() {
            "try_with_resources_statement" => {
                return parent.child_by_field_name("body") == Some(current);
            }
            "lambda_expression" | "method_declaration" | "class_body" => return false,
            _ => current = parent,
        }
    }
    false
}

/// Whether a type as written names one of the JDK's functional interfaces,
/// anywhere among its arguments: PIT's `StaticInitializerInterceptor` leaves
/// a lambda stored in such a field to be mutated.
fn names_functional_type(ty: &str) -> bool {
    ty.split(|c: char| !c.is_alphanumeric() && c != '_')
        .any(|word| {
            matches!(word, "Runnable" | "Callable" | "Comparator")
                || ["Function", "Supplier", "Consumer", "Predicate", "Operator"]
                    .iter()
                    .any(|suffix| word.ends_with(suffix))
        })
}

/// A type as written without its annotations: a type argument may not
/// carry a declaration annotation (`@Nullable String`).
fn without_annotations(ty: &str) -> String {
    let mut out = String::new();
    let mut chars = ty.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '@' {
            // The name, then any arguments in parentheses.
            while chars
                .peek()
                .is_some_and(|c| c.is_alphanumeric() || *c == '.' || *c == '_')
            {
                chars.next();
            }
            if chars.peek() == Some(&'(') {
                let mut depth = 0;
                for c in chars.by_ref() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A Kotlin function's return type as written, where it is.
fn kotlin_written_return(node: Node, source: &str) -> Option<String> {
    let mut after_parameters = false;
    for child in children(node) {
        match child.kind() {
            "function_value_parameters" => after_parameters = true,
            "function_body" => return None,
            kind if after_parameters
                && matches!(
                    kind,
                    "user_type" | "nullable_type" | "function_type" | "parenthesized_type"
                ) =>
            {
                return Some(text(child, source).to_owned());
            }
            _ => {}
        }
    }
    None
}

/// A probe in place of a binary operator: `helper(slot, left /*op*/, right)`,
/// the operator kept in a comment so that nothing is taken out of the
/// source and the probe composes with the coverage probes' insertions.
fn binary_probe(
    kind: ProbeKind,
    helper: &str,
    range: Range,
    left: Node,
    operator: Range,
    right: Node,
) -> ValueProbe {
    ValueProbe {
        kind,
        span: range,
        edits: vec![
            (
                left.start_byte(),
                opening_rank(range),
                format!("{PROBES}.{helper}(\u{2}, "),
            ),
            (operator.0, opening_rank(range), "/*".to_owned()),
            (operator.1, 10, "*/,".to_owned()),
            (right.end_byte(), 10, ")".to_owned()),
        ],
    }
}

/// A probe around a returned value: outside every coverage wrapper of it.
fn return_probe(value: Node, cast: Option<&str>) -> ValueProbe {
    let range = (value.start_byte(), value.end_byte());
    let (open, close) = match cast {
        Some(cast) => (format!("(({cast}) {PROBES}.wr(\u{2}, "), "))".to_owned()),
        None => (format!("{PROBES}.wr(\u{2}, "), ")".to_owned()),
    };
    ValueProbe {
        kind: ProbeKind::Return,
        span: range,
        edits: vec![(range.0, 1, open), (range.1, closing_rank_of(value), close)],
    }
}

/// How a returned value ends, as javac lays it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tail {
    /// `iconst_0` (or a long, float or double zero): `false`, `0`, a
    /// comparison or `&&` compiled to jumps.
    Zero,
    /// `iconst_1`: `true`, `1`.
    One,
    Null,
    /// An empty value PIT's filter knows: `""`, `Collections.emptyList()`.
    Empty,
    Other,
}

fn binary_operator(node: Node, source: &str) -> String {
    node.child_by_field_name("operator")
        .map(|op| text(op, source).to_owned())
        .unwrap_or_default()
}

/// The operator token between a binary expression's operands.
fn operator_range(node: Node, left: Node, right: Node, source: &str) -> (Range, String) {
    if let Some(op) = node.child_by_field_name("operator") {
        return (
            (op.start_byte(), op.end_byte()),
            text(op, source).to_owned(),
        );
    }
    // Kotlin's infix call: the identifier between the operands.
    for child in children(node) {
        if child.start_byte() >= left.end_byte() && child.end_byte() <= right.start_byte() {
            return (
                (child.start_byte(), child.end_byte()),
                text(child, source).to_owned(),
            );
        }
    }
    let gap = &source[left.end_byte()..right.start_byte()];
    let trimmed = gap.trim();
    let at = left.end_byte() + gap.find(trimmed).unwrap_or(0);
    ((at, at + trimmed.len()), trimmed.to_owned())
}

fn modifiers_text(node: Node, source: &str) -> String {
    named_children(node)
        .filter(|child| child.kind() == "modifiers")
        .map(|modifiers| text(modifiers, source).to_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Every method a member calls by its simple name, unqualified or on
/// `this`, as `name/arguments`: PIT follows a call to one overload.
fn collect_calls(node: Node, source: &str, out: &mut Vec<String>) {
    collect_calls_in(node, source, "", out);
}

/// `collect_calls` within the class named `class`: a constructor call is
/// its own class's only where it creates one.
fn collect_calls_in(node: Node, source: &str, class: &str, out: &mut Vec<String>) {
    let count = |arguments: Option<Node>| arguments.map_or(0, |a| named_children(a).count());
    if node.kind() == "method_invocation" {
        let qualified = node
            .child_by_field_name("object")
            .is_some_and(|object| !matches!(text(object, source), "this"));
        if !qualified && let Some(name) = node.child_by_field_name("name") {
            out.push(format!(
                "{}/{}",
                text(name, source),
                count(node.child_by_field_name("arguments"))
            ));
        }
    }
    if node.kind() == "object_creation_expression" {
        let created = node
            .child_by_field_name("type")
            .map(|ty| text(ty, source))
            .unwrap_or("");
        let created = created.split('<').next().unwrap_or(created);
        let created = created.rsplit('.').next().unwrap_or(created);
        if class.is_empty() || created == class {
            out.push(format!(
                "<init>/{}",
                count(node.child_by_field_name("arguments"))
            ));
        }
    }
    if node.kind() == "call_expression"
        && let Some(callee) = named_children(node).next()
        && callee.kind() == "identifier"
    {
        let arguments = named_children(node)
            .filter(|child| child.kind() == "value_arguments")
            .map(|arguments| named_children(arguments).count())
            .sum::<usize>()
            + named_children(node)
                .filter(|child| child.kind() == "annotated_lambda")
                .count();
        out.push(format!("{}/{arguments}", text(callee, source)));
    }
    for child in named_children(node) {
        collect_calls_in(child, source, class, out);
    }
}

/// Whether a call key (`name/arguments`) calls a declaration's key
/// (`name/parameters`, `name/*` for a variable count).
fn calls_declaration(call: &str, declaration: &str) -> bool {
    let (called, arguments) = call.rsplit_once('/').unwrap_or((call, "*"));
    let (declared, parameters) = declaration.rsplit_once('/').unwrap_or((declaration, "*"));
    called == declared && (parameters == "*" || arguments == parameters)
}

/// A declaration's key: its name and parameter count.
fn declaration_key(name: &str, arity: Option<usize>) -> String {
    match arity {
        Some(arity) => format!("{name}/{arity}"),
        None => format!("{name}/*"),
    }
}

/// The name a condition reads first, where that is a plain name.
fn first_read(condition: Node, source: &str) -> Option<String> {
    let mut node = strip_parens(condition);
    loop {
        match node.kind() {
            "identifier" => return Some(text(node, source).to_owned()),
            "binary_expression" => node = strip_parens(node.child_by_field_name("left")?),
            "update_expression" => node = named_children(node).next()?,
            _ => return None,
        }
    }
}

/// What a lambda passed to a method of this name returns, by the functional
/// interface the JDK's method takes.
fn functional_return(name: &str, body: impl FnOnce() -> Option<Ty>) -> Option<Ty> {
    Some(match name {
        "filter" | "anyMatch" | "allMatch" | "noneMatch" | "removeIf" | "takeWhile"
        | "dropWhile" | "test" => Ty::Prim(Prim::Boolean),
        "mapToInt" | "comparingInt" | "flatMapToInt" | "sort" | "sorted" | "max" | "min"
        | "thenComparingInt" | "applyAsInt" => Ty::Prim(Prim::Int),
        "mapToLong" | "comparingLong" | "thenComparingLong" => Ty::Prim(Prim::Long),
        "mapToDouble" | "comparingDouble" | "thenComparingDouble" => Ty::Prim(Prim::Double),
        "forEach" | "ifPresent" | "peek" | "forEachOrdered" | "forEachRemaining"
        | "ifPresentOrElse" | "execute" | "submit_runnable" => Ty::Void,
        "map" | "flatMap" | "computeIfAbsent" | "orElseGet" | "mapToObj" | "thenApply"
        | "supplyAsync" | "comparing" | "thenComparing" | "reduce" | "merge"
        | "computeIfPresent" | "compute" | "collect" | "orElseThrow" | "setAll" => {
            return match body() {
                Some(Ty::Prim(prim)) => Some(Ty::Boxed(prim)),
                Some(Ty::Unknown) | None => None,
                other => other,
            };
        }
        _ => return None,
    })
}

/// What a lambda assigned to a variable of this functional type returns.
fn functional_type_return(ty: &str, body: impl FnOnce() -> Option<Ty>) -> Option<Ty> {
    let base = ty.split('<').next().unwrap_or(ty).trim();
    let base = base.rsplit('.').next().unwrap_or(base);
    Some(match base {
        "Predicate" | "BiPredicate" | "IntPredicate" | "LongPredicate" | "DoublePredicate"
        | "BooleanSupplier" => Ty::Prim(Prim::Boolean),
        "Comparator"
        | "IntUnaryOperator"
        | "IntBinaryOperator"
        | "ToIntFunction"
        | "ToIntBiFunction"
        | "IntSupplier"
        | "LongToIntFunction"
        | "DoubleToIntFunction" => Ty::Prim(Prim::Int),
        "LongUnaryOperator" | "LongBinaryOperator" | "ToLongFunction" | "LongSupplier"
        | "IntToLongFunction" => Ty::Prim(Prim::Long),
        "DoubleUnaryOperator"
        | "DoubleBinaryOperator"
        | "ToDoubleFunction"
        | "DoubleSupplier"
        | "IntToDoubleFunction" => Ty::Prim(Prim::Double),
        "Runnable" | "Consumer" | "BiConsumer" | "IntConsumer" | "LongConsumer"
        | "DoubleConsumer" => Ty::Void,
        "Function" | "BiFunction" | "Supplier" | "UnaryOperator" | "BinaryOperator"
        | "Callable" | "IntFunction" => {
            let arguments = ty
                .split_once('<')
                .map(|(_, rest)| rest.trim_end_matches('>'));
            let last = arguments.and_then(|arguments| split_type_arguments(arguments).pop());
            return match last {
                Some(last) => match java_type(&last) {
                    Ty::Object(name) if name == "?" || name.len() == 1 => match body() {
                        Some(Ty::Prim(prim)) => Some(Ty::Boxed(prim)),
                        // A value of the caller's own type variable: what
                        // the interface's is inferred as, erased to Object.
                        Some(Ty::Object(body)) if is_type_variable(&body) => {
                            Some(Ty::Object("Object".into()))
                        }
                        _ => None,
                    },
                    ty => Some(ty),
                },
                None => None,
            };
        }
        _ => return None,
    })
}

/// `T`, `E`: a single capital letter, as type variables are named.
fn is_type_variable(name: &str) -> bool {
    name.len() == 1 && name.chars().all(|c| c.is_ascii_uppercase())
}

fn split_type_arguments(arguments: &str) -> Vec<String> {
    let mut depth = 0;
    let mut out = vec![String::new()];
    for c in arguments.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth -= 1,
            ',' if depth == 0 => {
                out.push(String::new());
                continue;
            }
            _ => {}
        }
        out.last_mut().expect("one").push(c);
    }
    out.into_iter().map(|part| part.trim().to_owned()).collect()
}

/// A numeric literal's value and type, Java's or Kotlin's spelling
/// (`10L`, `0x1F`, `1_000`, `2.5f`, `1e3`, `0b101u`).
fn number_literal(literal: &str) -> Option<(f64, Prim)> {
    let clean = literal.replace('_', "");
    let lower = clean.to_ascii_lowercase();
    let lower = lower.trim_end_matches(['u']);
    let is_hex = lower.starts_with("0x");
    let (body, prim) = if let Some(body) = lower.strip_suffix('l') {
        (body, Prim::Long)
    } else if !is_hex && let Some(body) = lower.strip_suffix('f') {
        (body, Prim::Float)
    } else if !is_hex && let Some(body) = lower.strip_suffix('d') {
        (body, Prim::Double)
    } else if !is_hex && (lower.contains('.') || lower.contains('e')) {
        (lower, Prim::Double)
    } else {
        (lower, Prim::Int)
    };
    let value = if let Some(hex) = body.strip_prefix("0x") {
        u64::from_str_radix(hex, 16).ok()? as f64
    } else if let Some(binary) = body.strip_prefix("0b") {
        u64::from_str_radix(binary, 2).ok()? as f64
    } else if body.len() > 1 && body.starts_with('0') && body.chars().all(|c| c.is_ascii_digit()) {
        u64::from_str_radix(&body[1..], 8).ok()? as f64
    } else {
        body.parse::<f64>().ok()?
    };
    Some((value, prim))
}

impl Enumerator<'_> {
    fn java_constant(&self, node: Node) -> bool {
        self.java_constant_value(node).is_some()
    }

    /// The value of a compile-time constant expression (JLS 15.29), where
    /// the file shows it is one.
    fn java_constant_value(&self, node: Node) -> Option<Constant> {
        let node = strip_parens(node);
        match node.kind() {
            "decimal_integer_literal"
            | "hex_integer_literal"
            | "octal_integer_literal"
            | "binary_integer_literal"
            | "decimal_floating_point_literal"
            | "hex_floating_point_literal" => {
                number_literal(text(node, self.source)).map(|(value, _)| Constant::Number(value))
            }
            "character_literal" => {
                let inner = text(node, self.source).trim_matches('\'');
                Some(Constant::Number(if inner == "\\0" {
                    0.0
                } else {
                    inner.chars().next().map_or(0.0, |c| c as u32 as f64)
                }))
            }
            "true" => Some(Constant::Bool(true)),
            "false" => Some(Constant::Bool(false)),
            "string_literal" | "text_block" => Some(Constant::Text),
            "identifier" => self.lookup(text(node, self.source))?.constant,
            "field_access" => {
                // `Outer.CONSTANT` or `this.CONSTANT` of this file's classes.
                let field = node.child_by_field_name("field")?;
                let object = node.child_by_field_name("object")?;
                let owner = text(object, self.source);
                let known = owner == "this" || self.classes.iter().any(|class| class.name == owner);
                if !known {
                    return None;
                }
                self.lookup(text(field, self.source))?.constant
            }
            "unary_expression" => {
                let operator = text(node.child_by_field_name("operator")?, self.source);
                let operand = self.java_constant_value(node.child_by_field_name("operand")?)?;
                match (operator, operand) {
                    ("-", Constant::Number(value)) => Some(Constant::Number(-value)),
                    ("+", number @ Constant::Number(_)) => Some(number),
                    ("~", Constant::Number(value)) => {
                        Some(Constant::Number(!(value as i64) as f64))
                    }
                    ("!", Constant::Bool(value)) => Some(Constant::Bool(!value)),
                    _ => None,
                }
            }
            "binary_expression" => {
                let operator = binary_operator(node, self.source);
                let left = self.java_constant_value(node.child_by_field_name("left")?)?;
                let right = self.java_constant_value(node.child_by_field_name("right")?)?;
                Some(match (left, right) {
                    (Constant::Number(a), Constant::Number(b)) => match operator.as_str() {
                        "+" => Constant::Number(a + b),
                        "-" => Constant::Number(a - b),
                        "*" => Constant::Number(a * b),
                        "/" if b != 0.0 => Constant::Number(a / b),
                        "%" if b != 0.0 => Constant::Number(a % b),
                        "<" => Constant::Bool(a < b),
                        "<=" => Constant::Bool(a <= b),
                        ">" => Constant::Bool(a > b),
                        ">=" => Constant::Bool(a >= b),
                        "==" => Constant::Bool(a == b),
                        "!=" => Constant::Bool(a != b),
                        "&" => Constant::Number(((a as i64) & (b as i64)) as f64),
                        "|" => Constant::Number(((a as i64) | (b as i64)) as f64),
                        "^" => Constant::Number(((a as i64) ^ (b as i64)) as f64),
                        "<<" => Constant::Number(((a as i64).wrapping_shl(b as u32)) as f64),
                        ">>" | ">>>" => {
                            Constant::Number(((a as i64).wrapping_shr(b as u32)) as f64)
                        }
                        _ => return None,
                    },
                    (Constant::Bool(a), Constant::Bool(b)) => match operator.as_str() {
                        "&&" | "&" => Constant::Bool(a && b),
                        "||" | "|" => Constant::Bool(a || b),
                        "^" | "!=" => Constant::Bool(a != b),
                        "==" => Constant::Bool(a == b),
                        _ => return None,
                    },
                    (Constant::Text, _) | (_, Constant::Text) if operator == "+" => Constant::Text,
                    _ => return None,
                })
            }
            "ternary_expression" => {
                let condition = self.java_constant_value(node.child_by_field_name("condition")?)?;
                let consequence =
                    self.java_constant_value(node.child_by_field_name("consequence")?)?;
                let alternative =
                    self.java_constant_value(node.child_by_field_name("alternative")?)?;
                Some(if condition == Constant::Bool(true) {
                    consequence
                } else {
                    alternative
                })
            }
            "cast_expression" => {
                let ty = java_type(text(node.child_by_field_name("type")?, self.source));
                if !matches!(ty, Ty::Prim(_) | Ty::Str) {
                    return None;
                }
                let value = self.java_constant_value(node.child_by_field_name("value")?)?;
                Some(match (ty, value) {
                    (Ty::Prim(prim), Constant::Number(value))
                        if prim != Prim::Float && prim != Prim::Double =>
                    {
                        Constant::Number(value.trunc())
                    }
                    (_, value) => value,
                })
            }
            _ => None,
        }
    }

    /// The type of an expression, as far as the file shows it.
    fn java_type_of(&self, node: Node) -> Ty {
        let node = strip_parens(node);
        match node.kind() {
            "decimal_integer_literal"
            | "hex_integer_literal"
            | "octal_integer_literal"
            | "binary_integer_literal"
            | "decimal_floating_point_literal"
            | "hex_floating_point_literal" => number_literal(text(node, self.source))
                .map_or(Ty::Unknown, |(_, prim)| Ty::Prim(prim)),
            "character_literal" => Ty::Prim(Prim::Char),
            "true" | "false" => Ty::Prim(Prim::Boolean),
            "string_literal" | "text_block" => Ty::Str,
            "null_literal" => Ty::Null,
            "identifier" => self
                .lookup(text(node, self.source))
                .map_or(Ty::Unknown, |var| var.ty.clone()),
            "field_access" => {
                let object = node.child_by_field_name("object");
                let field = node.child_by_field_name("field");
                match (object, field) {
                    (Some(object), Some(field)) if text(object, self.source) == "this" => self
                        .lookup(text(field, self.source))
                        .map_or(Ty::Unknown, |var| var.ty.clone()),
                    (Some(object), Some(field)) if text(field, self.source) == "length" => {
                        match self.java_type_of(object) {
                            Ty::Object(name) if name.ends_with(']') => Ty::Prim(Prim::Int),
                            _ => Ty::Unknown,
                        }
                    }
                    _ => Ty::Unknown,
                }
            }
            "array_access" => {
                let array = node
                    .child_by_field_name("array")
                    .map_or(Ty::Unknown, |array| self.java_type_of(array));
                match array {
                    Ty::Object(name) if name.ends_with("[]") => {
                        java_type(name.strip_suffix("[]").unwrap_or(&name))
                    }
                    _ => Ty::Unknown,
                }
            }
            "cast_expression" => node
                .child_by_field_name("type")
                .map_or(Ty::Unknown, |ty| java_type(text(ty, self.source))),
            "object_creation_expression" => node
                .child_by_field_name("type")
                .map_or(Ty::Unknown, |ty| java_type(text(ty, self.source))),
            "array_creation_expression" => Ty::Object("[]".into()),
            "instanceof_expression" => Ty::Prim(Prim::Boolean),
            "ternary_expression" => {
                let consequence = node
                    .child_by_field_name("consequence")
                    .map_or(Ty::Unknown, |n| self.java_type_of(n));
                let alternative = node
                    .child_by_field_name("alternative")
                    .map_or(Ty::Unknown, |n| self.java_type_of(n));
                match (&consequence, &alternative) {
                    (Ty::Prim(a), Ty::Prim(b)) if a.numeric() && b.numeric() => {
                        Ty::Prim(a.promote(*b))
                    }
                    (Ty::Null, other) | (other, Ty::Null) => other.clone(),
                    _ if consequence == alternative => consequence,
                    (Ty::Unknown, other) | (other, Ty::Unknown) => other.clone(),
                    _ => Ty::Unknown,
                }
            }
            "assignment_expression" => node
                .child_by_field_name("left")
                .map_or(Ty::Unknown, |left| self.java_type_of(left)),
            "update_expression" => named_children(node)
                .next()
                .map_or(Ty::Unknown, |target| self.java_type_of(target)),
            "unary_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .map(|op| text(op, self.source))
                    .unwrap_or("");
                if operator == "!" {
                    return Ty::Prim(Prim::Boolean);
                }
                match node
                    .child_by_field_name("operand")
                    .map(|operand| self.java_type_of(operand))
                    .and_then(|ty| ty.prim())
                {
                    Some(prim) => Ty::Prim(prim.promote(Prim::Int)),
                    None => Ty::Unknown,
                }
            }
            "binary_expression" => {
                let operator = binary_operator(node, self.source);
                if matches!(
                    operator.as_str(),
                    "<" | "<=" | ">" | ">=" | "==" | "!=" | "&&" | "||"
                ) {
                    return Ty::Prim(Prim::Boolean);
                }
                let left = node
                    .child_by_field_name("left")
                    .map_or(Ty::Unknown, |n| self.java_type_of(n));
                let right = node
                    .child_by_field_name("right")
                    .map_or(Ty::Unknown, |n| self.java_type_of(n));
                if operator == "+"
                    && (left == Ty::Str
                        || right == Ty::Str
                        || matches!(left, Ty::Object(_) | Ty::Null)
                        || matches!(right, Ty::Object(_) | Ty::Null))
                {
                    return Ty::Str;
                }
                if matches!(operator.as_str(), "<<" | ">>" | ">>>") {
                    return left
                        .prim()
                        .map_or(Ty::Unknown, |prim| Ty::Prim(prim.promote(Prim::Int)));
                }
                match (left.prim(), right.prim()) {
                    (Some(Prim::Boolean), _) | (_, Some(Prim::Boolean)) => Ty::Prim(Prim::Boolean),
                    (Some(a), Some(b)) => Ty::Prim(a.promote(b)),
                    _ => Ty::Unknown,
                }
            }
            "method_invocation" => {
                let name = node
                    .child_by_field_name("name")
                    .map(|name| text(name, self.source))
                    .unwrap_or("");
                let object = node
                    .child_by_field_name("object")
                    .map(|object| text(object, self.source));
                if let Some(known) = known_method_type(object, name) {
                    return known;
                }
                self.index.return_type(name).cloned().unwrap_or(Ty::Unknown)
            }
            "lambda_expression" | "method_reference" => Ty::Object("Function".into()),
            "this" => Ty::Object(
                self.classes
                    .last()
                    .map(|class| class.name.clone())
                    .unwrap_or_default(),
            ),
            _ => Ty::Unknown,
        }
    }
}

/// The JDK methods whose result's type the source names: enough to tell a
/// string concatenation from an addition.
fn known_method_type(object: Option<&str>, name: &str) -> Option<Ty> {
    match (object, name) {
        (Some("String"), "valueOf" | "format" | "join") => Some(Ty::Str),
        (Some("Integer"), "parseInt" | "compare" | "signum" | "bitCount") => {
            Some(Ty::Prim(Prim::Int))
        }
        (Some("Long"), "parseLong") => Some(Ty::Prim(Prim::Long)),
        (Some("Double"), "parseDouble") => Some(Ty::Prim(Prim::Double)),
        (Some("Math"), "abs" | "max" | "min") => None,
        (Some("Math"), "floorDiv" | "floorMod" | "round" | "toIntExact" | "addExact") => None,
        (Some("Math"), _) => Some(Ty::Prim(Prim::Double)),
        (
            _,
            "toString" | "name" | "getName" | "getSimpleName" | "toUpperCase" | "toLowerCase"
            | "trim" | "strip" | "substring" | "getMessage" | "repeat" | "replace" | "replaceAll"
            | "concat" | "toPlainString" | "getKey" | "nextLine",
        ) => {
            if name == "getKey" {
                return None;
            }
            Some(Ty::Str)
        }
        (
            _,
            "size" | "length" | "indexOf" | "lastIndexOf" | "hashCode" | "compareTo" | "ordinal"
            | "intValue" | "nextInt" | "getAsInt" | "available",
        ) => Some(Ty::Prim(Prim::Int)),
        (_, "longValue" | "nanoTime" | "currentTimeMillis" | "getAsLong") => {
            Some(Ty::Prim(Prim::Long))
        }
        (_, "doubleValue" | "nextDouble" | "getAsDouble") => Some(Ty::Prim(Prim::Double)),
        (_, "floatValue") => Some(Ty::Prim(Prim::Float)),
        (_, "charAt") => Some(Ty::Prim(Prim::Char)),
        (
            _,
            "equals" | "isEmpty" | "contains" | "startsWith" | "endsWith" | "hasNext"
            | "containsKey" | "equalsIgnoreCase" | "matches" | "isPresent" | "booleanValue",
        ) => Some(Ty::Prim(Prim::Boolean)),
        _ => None,
    }
}

// --- Kotlin ------------------------------------------------------------------------

/// Whether `node` is the receiver of the call, navigation or index chain
/// around it.
fn kotlin_chain_receiver(node: Node) -> bool {
    node.parent().is_some_and(|parent| {
        matches!(
            parent.kind(),
            "call_expression" | "navigation_expression" | "index_expression"
        ) && named_children(parent)
            .next()
            .is_some_and(|first| first.id() == node.id())
    })
}

/// Whether `name`, declared by `declaration`, is read in a lambda among the
/// statements after it.
fn kotlin_read_in_a_later_lambda(declaration: Node, name: &str, source: &str) -> bool {
    fn names_it(node: Node, name: &str, source: &str, in_lambda: bool) -> bool {
        let in_lambda = in_lambda || node.kind() == "lambda_literal";
        if in_lambda && node.kind() == "identifier" && text(node, source) == name {
            return true;
        }
        // `"$name"`: the grammar leaves a bare template in the string.
        if in_lambda && node.kind().ends_with("string_literal") {
            let template = format!("${name}");
            let string = text(node, source);
            if string.match_indices(&template).any(|(at, _)| {
                !string[at + template.len()..]
                    .starts_with(|c: char| c.is_alphanumeric() || c == '_')
            }) {
                return true;
            }
        }
        named_children(node).any(|child| names_it(child, name, source, in_lambda))
    }
    let mut next = declaration.next_named_sibling();
    while let Some(statement) = next {
        if names_it(statement, name, source, false) {
            return true;
        }
        next = statement.next_named_sibling();
    }
    false
}

/// Kotlin's standard library functions that are `inline`: a call of one is
/// not a call in the bytecode, so PIT has no call to remove.
const KOTLIN_INLINE: &[&str] = &[
    "forEach",
    "forEachIndexed",
    "repeat",
    "require",
    "check",
    "requireNotNull",
    "checkNotNull",
    "run",
    "also",
    "apply",
    "let",
    "with",
    "onEach",
    "synchronized",
    "use",
    "measureTimeMillis",
    "measureNanoTime",
    "assert",
    "takeIf",
    "takeUnless",
    "buildString",
    "buildList",
    "buildMap",
    "buildSet",
    "error",
    "TODO",
    "lazy",
    "map",
    "filter",
    "forEachLine",
    "withLock",
    "runCatching",
    "onSuccess",
    "onFailure",
    "getOrElse",
    "ifEmpty",
    "orEmpty",
    "contract",
];

impl Enumerator<'_> {
    fn kotlin_file(&mut self, root: Node) {
        for child in named_children(root) {
            match child.kind() {
                "package_header" => {
                    self.package = named_children(child)
                        .find(|name| matches!(name.kind(), "qualified_identifier" | "identifier"))
                        .map(|name| text(name, self.source).to_owned())
                        .unwrap_or_default();
                }
                _ => self.kotlin_declaration(child, false),
            }
        }
    }

    /// A declaration in a file, class or object body. `static_context` for
    /// one whose initialisation runs in a static initializer (an `object`'s,
    /// a companion's, the file's top level).
    fn kotlin_declaration(&mut self, node: Node, static_context: bool) {
        match node.kind() {
            "class_declaration" | "object_declaration" | "companion_object" => {
                let name = named_children(node)
                    .find(|child| child.kind() == "identifier")
                    .map(|name| text(name, self.source).to_owned())
                    .unwrap_or_else(|| "Companion".to_owned());
                let object = node.kind() != "class_declaration";
                let enumeration = text(node, self.source).trim_start().starts_with("enum")
                    || modifiers_text(node, self.source).contains("enum");
                let outer_methods = std::mem::take(&mut self.methods);
                self.classes.push(Class {
                    name,
                    ..Class::default()
                });
                // The primary constructor's properties are fields.
                if let Some(constructor) =
                    named_children(node).find(|child| child.kind() == "primary_constructor")
                {
                    self.scopes.push(HashMap::new());
                    for parameter in named_children(constructor)
                        .filter(|child| child.kind() == "class_parameters")
                        .flat_map(named_children)
                        .filter(|child| child.kind() == "class_parameter")
                    {
                        let name = named_children(parameter)
                            .find(|child| child.kind() == "identifier")
                            .map(|name| text(name, self.source).to_owned());
                        let ty = named_children(parameter)
                            .find(|child| child.kind().ends_with("type"))
                            .map_or(Ty::Unknown, |ty| kotlin_type(text(ty, self.source)));
                        if let Some(name) = name
                            && let Some(class) = self.classes.last_mut()
                        {
                            class.fields.insert(
                                name,
                                Var {
                                    ty,
                                    local: false,
                                    constant: None,
                                },
                            );
                        }
                    }
                    self.scopes.pop();
                }
                for body in named_children(node)
                    .filter(|child| matches!(child.kind(), "class_body" | "enum_class_body"))
                {
                    self.kotlin_class_body(body, object || static_context, enumeration);
                }
                self.classes.pop();
                self.methods = outer_methods;
            }
            "function_declaration" => self.kotlin_function(node),
            "property_declaration" => {
                let is_const = modifiers_text(node, self.source).contains("const");
                let ty = self.kotlin_property_type(node);
                let constant = if is_const {
                    named_children(node)
                        .filter(|child| {
                            !matches!(child.kind(), "modifiers" | "variable_declaration")
                        })
                        .last()
                        .and_then(|value| self.kotlin_constant_value(value))
                        .or(Some(Constant::Text))
                } else {
                    None
                };
                let name = named_children(node)
                    .find(|child| child.kind() == "variable_declaration")
                    .and_then(|variable| named_children(variable).next())
                    .map(|name| text(name, self.source).to_owned());
                if let Some(name) = &name {
                    let var = Var {
                        ty: ty.clone(),
                        local: false,
                        constant,
                    };
                    match self.classes.last_mut() {
                        Some(class) => {
                            class.fields.insert(name.clone(), var);
                        }
                        None => {
                            if self.scopes.is_empty() {
                                self.scopes.push(HashMap::new());
                            }
                            self.declare(name, var);
                        }
                    }
                }
                // An initializer runs in the constructor, or in the static
                // initializer for an object's or the file's properties.
                let is_static = static_context || self.classes.is_empty() || is_const;
                self.methods.push(Method {
                    name: if is_static { "<clinit>" } else { "<init>" }.to_owned(),
                    returns: None,
                    not_null: false,
                    static_init: is_static,
                    functional_field: false,
                    written_returns: None,
                });
                self.scopes.push(HashMap::new());
                for child in named_children(node) {
                    match child.kind() {
                        "modifiers" | "variable_declaration" | "type_constraints" => {}
                        "getter" | "setter" => {
                            self.methods.pop();
                            self.kotlin_accessor(
                                child,
                                name.as_deref().unwrap_or_default(),
                                &ty,
                                nullable_property(node, self.source),
                            );
                            self.methods.push(Method {
                                name: "<init>".to_owned(),
                                returns: None,
                                not_null: false,
                                static_init: is_static,
                                functional_field: false,
                                written_returns: None,
                            });
                        }
                        "property_delegate" => {}
                        _ => self.kotlin_node(child, Context::Value),
                    }
                }
                self.scopes.pop();
                self.methods.pop();
            }
            "anonymous_initializer" => {
                self.methods.push(Method {
                    name: if static_context { "<clinit>" } else { "<init>" }.to_owned(),
                    returns: Some(Ty::Void),
                    not_null: false,
                    static_init: static_context,
                    functional_field: false,
                    written_returns: None,
                });
                self.kotlin_children(node, Context::Statement);
                self.methods.pop();
            }
            "secondary_constructor" => {
                self.methods.push(Method {
                    name: "<init>".to_owned(),
                    returns: Some(Ty::Void),
                    not_null: false,
                    static_init: static_context,
                    functional_field: false,
                    written_returns: None,
                });
                self.scopes.push(HashMap::new());
                for child in named_children(node) {
                    match child.kind() {
                        "function_value_parameters" => self.kotlin_parameters(child),
                        "block" => self.kotlin_node(child, Context::Statement),
                        "constructor_delegation_call" => {
                            self.kotlin_children(child, Context::Value)
                        }
                        _ => {}
                    }
                }
                self.scopes.pop();
                self.methods.pop();
            }
            _ => {}
        }
    }

    fn kotlin_class_body(&mut self, body: Node, static_context: bool, enumeration: bool) {
        let members = named_children(body).collect::<Vec<_>>();
        // Fields first, so that a function above a property knows its type.
        for member in &members {
            if member.kind() == "property_declaration" {
                let ty = self.kotlin_property_type(*member);
                if let Some(name) = named_children(*member)
                    .find(|child| child.kind() == "variable_declaration")
                    .and_then(|variable| named_children(variable).next())
                {
                    let name = text(name, self.source).to_owned();
                    if let Some(class) = self.classes.last_mut() {
                        class.fields.insert(
                            name,
                            Var {
                                ty,
                                local: false,
                                constant: None,
                            },
                        );
                    }
                }
            }
        }
        // Private functions an object's initialisation calls are
        // initialisation too (PIT's `StaticInitializerInterceptor`).
        if static_context {
            let private = members
                .iter()
                .filter(|member| {
                    member.kind() == "function_declaration"
                        && modifiers_text(**member, self.source).contains("private")
                })
                .filter_map(|member| {
                    let name =
                        named_children(*member).find(|child| child.kind() == "identifier")?;
                    let parameters = named_children(*member)
                        .find(|child| child.kind() == "function_value_parameters");
                    let arity = match parameters {
                        Some(parameters) if text(parameters, self.source).contains('=') => None,
                        Some(parameters) => Some(
                            named_children(parameters)
                                .filter(|child| child.kind() == "parameter")
                                .count(),
                        ),
                        None => Some(0),
                    };
                    Some((declaration_key(text(name, self.source), arity), *member))
                })
                .collect::<Vec<_>>();
            let mut pending = Vec::new();
            for member in &members {
                if matches!(
                    member.kind(),
                    "property_declaration" | "anonymous_initializer"
                ) {
                    collect_calls(*member, self.source, &mut pending);
                }
            }
            let mut initializing = BTreeSet::new();
            while let Some(name) = pending.pop() {
                let bodies = private
                    .iter()
                    .filter(|(key, _)| calls_declaration(&name, key) && !initializing.contains(key))
                    .collect::<Vec<_>>();
                for (key, body) in bodies {
                    initializing.insert(key.clone());
                    collect_calls(*body, self.source, &mut pending);
                }
            }
            if let Some(class) = self.classes.last_mut() {
                class.initializing = initializing;
            }
        }
        let _ = enumeration;
        for member in members {
            match member.kind() {
                "enum_entry" => {
                    for body in named_children(member).filter(|child| child.kind() == "class_body")
                    {
                        let position = self.classes.last().map_or(0, |class| class.lambdas) + 1;
                        self.classes.push(Class {
                            name: position.to_string(),
                            ..Class::default()
                        });
                        self.kotlin_class_body(body, false, false);
                        self.classes.pop();
                    }
                }
                "companion_object" => self.kotlin_declaration(member, true),
                _ => self.kotlin_declaration(member, static_context),
            }
        }
    }

    fn kotlin_property_type(&self, node: Node) -> Ty {
        let declared = named_children(node)
            .find(|child| child.kind() == "variable_declaration")
            .and_then(|variable| {
                named_children(variable).find(|child| child.kind().ends_with("type"))
            });
        if let Some(declared) = declared {
            return kotlin_type(text(declared, self.source));
        }
        named_children(node)
            .filter(|child| {
                !matches!(
                    child.kind(),
                    "modifiers"
                        | "variable_declaration"
                        | "getter"
                        | "setter"
                        | "property_delegate"
                )
            })
            .last()
            .map_or(Ty::Unknown, |value| self.kotlin_type_of(value))
    }

    /// A property's `get()` or `set(value)`: the getter returns the
    /// property's type, as the method kotlinc writes for it does.
    fn kotlin_accessor(&mut self, accessor: Node, property: &str, ty: &Ty, nullable: bool) {
        let getter = accessor.kind() == "getter";
        // The JVM name kotlinc gives it: `getName`, `setName`, and `isOpen`
        // for a property named `isOpen`.
        let capitalized = property
            .chars()
            .next()
            .map(|first| {
                first
                    .to_uppercase()
                    .chain(property.chars().skip(1))
                    .collect::<String>()
            })
            .unwrap_or_default();
        let jvm_name = if getter && property.starts_with("is") && property.len() > 2 {
            property.to_owned()
        } else if getter {
            format!("get{capitalized}")
        } else if property.starts_with("is") && property.len() > 2 {
            format!("set{}", &property[2..])
        } else {
            format!("set{capitalized}")
        };
        let returns = if !getter {
            Some(Ty::Void)
        } else if *ty == Ty::Unknown {
            None
        } else {
            Some(ty.clone())
        };
        self.methods.push(Method {
            name: jvm_name,
            returns,
            not_null: !nullable,
            static_init: self.suppressed_context(),
            functional_field: false,
            written_returns: None,
        });
        self.scopes.push(HashMap::new());
        // `get() = value`: the value is what it returns.
        if getter
            && let Some(body) =
                named_children(accessor).find(|child| child.kind() == "function_body")
            && text(body, self.source).trim_start().starts_with('=')
            && let Some(value) = named_children(body).next()
        {
            self.kotlin_return(value, value);
        }
        self.kotlin_children(accessor, Context::Statement);
        self.scopes.pop();
        self.methods.pop();
    }

    fn suppressed_context(&self) -> bool {
        self.methods.last().is_some_and(|method| method.static_init)
    }

    fn kotlin_function(&mut self, node: Node) {
        let name = named_children(node)
            .find(|child| child.kind() == "identifier")
            .map(|name| text(name, self.source).to_owned())
            .unwrap_or_default();
        let declared = kotlin_declared_return(node, self.source);
        let body = named_children(node).find(|child| child.kind() == "function_body");
        let expression_body = body.and_then(|body| {
            text(body, self.source)
                .trim_start()
                .starts_with('=')
                .then(|| named_children(body).last())
                .flatten()
        });
        let arity = named_children(node)
            .find(|child| child.kind() == "function_value_parameters")
            .map_or(Some(0), |parameters| {
                if text(parameters, self.source).contains('=') {
                    None
                } else {
                    Some(
                        named_children(parameters)
                            .filter(|child| child.kind() == "parameter")
                            .count(),
                    )
                }
            });
        let key = declaration_key(&name, arity);
        let initializing = self
            .classes
            .last()
            .is_some_and(|class| class.initializing.contains(&key));
        let modifiers = modifiers_text(node, self.source);
        let abstract_or_external = body.is_none();
        // `Any`'s own members return what `Any` declares, whatever the
        // expression body's type reads as here (`hash(a, b)`, a function
        // of the project's or a library's).
        let any_member = match (name.as_str(), arity) {
            ("hashCode", Some(0)) => Some("Int"),
            ("equals", Some(1)) => Some("Boolean"),
            ("toString", Some(0)) => Some("String"),
            _ => None,
        }
        .filter(|_| modifiers.contains("override"));
        self.methods.push(Method {
            name,
            returns: None,
            not_null: false,
            static_init: initializing || abstract_or_external,
            functional_field: false,
            written_returns: kotlin_written_return(node, self.source),
        });
        self.scopes.push(HashMap::new());
        if let Some(parameters) =
            named_children(node).find(|child| child.kind() == "function_value_parameters")
        {
            self.kotlin_parameters(parameters);
        }
        // An expression body's type is the expression's where none is
        // written; known once the parameters are in scope.
        let returns = match (&declared, expression_body, any_member) {
            (Ty::Unknown, Some(_), Some(declared_by_any)) => kotlin_type(declared_by_any),
            (Ty::Unknown, Some(expression), None) => self.kotlin_type_of(expression),
            _ => declared,
        };
        // Kotlin marks a non-null reference return `@NotNull`, which PIT
        // reads as a promise that `null` is never returned.
        let nullable = kotlin_declared_nullable(node, self.source);
        let not_null = !nullable || modifiers.contains("@NotNull");
        {
            let method = self.methods.last_mut().expect("pushed");
            method.returns = Some(returns);
            method.not_null = not_null;
        }
        if let Some(body) = body {
            if let Some(expression) = expression_body {
                self.kotlin_return(expression, expression);
                self.kotlin_value_statement(expression);
            } else {
                self.kotlin_children(body, Context::Statement);
            }
        }
        self.scopes.pop();
        self.methods.pop();
    }

    fn kotlin_parameters(&mut self, parameters: Node) {
        for parameter in named_children(parameters).filter(|child| child.kind() == "parameter") {
            let name = named_children(parameter).find(|child| child.kind() == "identifier");
            let ty = named_children(parameter)
                .find(|child| child.kind().ends_with("type"))
                .map_or(Ty::Unknown, |ty| kotlin_type(text(ty, self.source)));
            if let Some(name) = name {
                self.declare(
                    text(name, self.source),
                    Var {
                        ty,
                        local: true,
                        constant: None,
                    },
                );
            }
        }
    }

    fn kotlin_children(&mut self, node: Node, context: Context) {
        for child in named_children(node) {
            self.kotlin_node(child, context);
        }
    }

    /// An expression whose value is the function's: walked as a value, and
    /// as a statement where it is a call (a `Unit` function's body).
    fn kotlin_value_statement(&mut self, expression: Node) {
        let returns_unit = self
            .method()
            .and_then(|method| method.returns.clone())
            .is_some_and(|returns| returns == Ty::Void);
        if returns_unit {
            self.kotlin_statement(expression);
        } else {
            self.kotlin_node(expression, Context::Value);
        }
    }

    /// A statement in a block or a lambda: a call statement may be removed.
    fn kotlin_statement(&mut self, node: Node) {
        if node.kind() == "call_expression" {
            self.kotlin_call_statement(node);
        }
        self.kotlin_node(node, Context::Statement);
    }

    fn kotlin_call_statement(&mut self, call: Node) {
        let Some(callee) = named_children(call).next() else {
            return;
        };
        let (name, safe) = match callee.kind() {
            "identifier" => (text(callee, self.source).to_owned(), false),
            "navigation_expression" => {
                let name = named_children(callee)
                    .last()
                    .map(|name| text(name, self.source).to_owned())
                    .unwrap_or_default();
                let safe = children(callee).any(|child| child.kind() == "?.");
                (name, safe)
            }
            _ => return,
        };
        if name.is_empty() || KOTLIN_INLINE.contains(&name.as_str()) || self.index.inline(&name) {
            return;
        }
        // A constructor call is not a method call.
        if name.chars().next().is_some_and(char::is_uppercase) {
            return;
        }
        let arguments = named_children(call)
            .filter(|child| child.kind() == "value_arguments")
            .map(|arguments| named_children(arguments).count())
            .sum::<usize>()
            + named_children(call)
                .filter(|child| child.kind() == "annotated_lambda")
                .count();
        let receiver = match callee.kind() {
            "navigation_expression" => named_children(callee).next().and_then(|object| match self
                .kotlin_type_of(object)
            {
                Ty::Object(name) => Some(name),
                Ty::Str => Some("Str".to_owned()),
                _ => None,
            }),
            _ => self.classes.last().map(|class| class.name.clone()),
        };
        if self.index.void_call(&name, arguments, receiver.as_deref()) != Some(true) {
            return;
        }
        let _ = safe;
        let range = (call.start_byte(), call.end_byte());
        let lines = (self.line(range.0), self.line(range.1));
        self.push(
            Mutator::VoidMethodCall,
            "removed",
            range,
            String::new(),
            None,
            Placement {
                span: range,
                holes: vec![range],
                form: Form::RemoveCall { lambda: false },
                layer: 0,
            },
            name,
            lines,
        );
    }

    fn kotlin_node(&mut self, node: Node, context: Context) {
        match node.kind() {
            "line_comment" | "block_comment" | "annotation" | "modifiers" | "user_type"
            | "nullable_type" | "type_arguments" | "label" => {}
            "class_declaration" | "object_declaration" => {
                self.kotlin_declaration(node, false);
            }
            "function_declaration" => self.kotlin_function(node),
            "object_literal" => {
                let outer_methods = std::mem::take(&mut self.methods);
                let static_init = outer_methods.last().is_some_and(|m| m.static_init);
                let position = self.classes.last().map_or(0, |class| class.lambdas) + 1;
                self.classes.push(Class {
                    name: position.to_string(),
                    ..Class::default()
                });
                for body in named_children(node).filter(|child| child.kind() == "class_body") {
                    self.kotlin_class_body(body, static_init, false);
                }
                self.classes.pop();
                self.methods = outer_methods;
            }
            "property_declaration" => {
                // A local.
                let ty = self.kotlin_property_type(node);
                for child in named_children(node) {
                    if !matches!(
                        child.kind(),
                        "modifiers" | "variable_declaration" | "multi_variable_declaration"
                    ) {
                        self.kotlin_node(child, Context::Value);
                    }
                }
                if let Some(name) = named_children(node)
                    .find(|child| child.kind() == "variable_declaration")
                    .and_then(|variable| named_children(variable).next())
                {
                    let name = text(name, self.source).to_owned();
                    // A local a lambda reads (inlined or not) is one kotlinc
                    // increments by loading, adding and storing, never with
                    // `IINC`: no local `int` for PIT's `Increments`.
                    let captured = kotlin_read_in_a_later_lambda(node, &name, self.source);
                    self.declare(
                        &name,
                        Var {
                            ty,
                            local: !captured,
                            constant: None,
                        },
                    );
                }
            }
            "block" | "statements" => {
                self.scopes.push(HashMap::new());
                for child in named_children(node) {
                    self.kotlin_statement(child);
                }
                self.scopes.pop();
            }
            "lambda_literal" => {
                // Kotlin's lambdas passed to inline functions are inlined:
                // their code is the enclosing method's. A lambda's last
                // expression is its value.
                self.scopes.push(HashMap::new());
                let enclosing = self.methods.last().cloned();
                let inline = node
                    .parent()
                    .and_then(|parent| parent.parent())
                    .and_then(|call| named_children(call).next())
                    .map(|callee| {
                        let name = named_children(callee)
                            .last()
                            .map_or(text(callee, self.source), |name| text(name, self.source));
                        KOTLIN_INLINE.contains(&name) || kotlin_collection_inline(name)
                    })
                    .unwrap_or(false);
                let deferred = !inline;
                if deferred {
                    self.deferred.push((node.start_byte(), node.end_byte()));
                }
                if let Some(enclosing) = enclosing {
                    self.methods.push(Method {
                        name: if inline {
                            enclosing.name.clone()
                        } else {
                            format!("{}$lambda", enclosing.name)
                        },
                        returns: None,
                        not_null: true,
                        static_init: enclosing.static_init,
                        functional_field: false,
                        written_returns: None,
                    });
                }
                if let Some(parameters) =
                    named_children(node).find(|child| child.kind() == "lambda_parameters")
                {
                    for parameter in named_children(parameters) {
                        let name = if parameter.kind() == "variable_declaration" {
                            named_children(parameter).next()
                        } else {
                            Some(parameter)
                        };
                        if let Some(name) = name {
                            self.declare(
                                text(name, self.source),
                                Var {
                                    ty: Ty::Unknown,
                                    local: true,
                                    constant: None,
                                },
                            );
                        }
                    }
                }
                let statements = named_children(node)
                    .filter(|child| child.kind() != "lambda_parameters")
                    .collect::<Vec<_>>();
                let count = statements.len();
                for (position, statement) in statements.into_iter().enumerate() {
                    if statement.kind() == "statements" {
                        let inner = named_children(statement).collect::<Vec<_>>();
                        let last = inner.len().saturating_sub(1);
                        for (index, child) in inner.into_iter().enumerate() {
                            if index == last {
                                self.kotlin_node(child, Context::Value);
                            } else {
                                self.kotlin_statement(child);
                            }
                        }
                    } else if position + 1 == count {
                        self.kotlin_node(statement, Context::Value);
                    } else {
                        self.kotlin_statement(statement);
                    }
                }
                if !self.methods.is_empty() && node.parent().is_some() {
                    self.methods.pop();
                }
                self.scopes.pop();
                if deferred {
                    self.deferred.pop();
                }
            }
            "return_expression" => {
                if let Some(value) = named_children(node).find(|child| child.kind() != "label") {
                    let labelled = text(node, self.source).starts_with("return@");
                    if !labelled {
                        self.kotlin_return(node, value);
                    }
                    self.kotlin_node(value, Context::Value);
                }
            }
            "if_expression" => {
                let condition = node.child_by_field_name("condition");
                let constant =
                    condition.and_then(|condition| self.kotlin_constant_value(condition));
                let branches = named_children(node)
                    .filter(|child| Some(*child) != condition)
                    .collect::<Vec<_>>();
                if let Some(condition) = condition {
                    self.kotlin_node(condition, Context::Condition);
                }
                for (position, branch) in branches.into_iter().enumerate() {
                    let dead = match constant {
                        Some(Constant::Bool(false)) => position == 0,
                        Some(Constant::Bool(true)) => position == 1,
                        _ => false,
                    };
                    if dead {
                        continue;
                    }
                    if context == Context::Statement {
                        self.kotlin_statement(branch);
                    } else {
                        self.kotlin_node(branch, context);
                    }
                }
            }
            "while_statement" | "do_while_statement" => {
                let condition = node.child_by_field_name("condition");
                for child in named_children(node) {
                    if Some(child) == condition {
                        self.kotlin_node(child, Context::Condition);
                    } else {
                        self.kotlin_statement(child);
                    }
                }
                if node.kind() == "while_statement" {
                    self.kotlin_loop_counter(node);
                }
            }
            "for_statement" => {
                // The compiler's own loop over a range or an iterator: its
                // counter and its condition have no source to negate.
                self.scopes.push(HashMap::new());
                for child in named_children(node) {
                    match child.kind() {
                        "variable_declaration" | "multi_variable_declaration" => {
                            // A loop over an `Int` range (`0 until n`, `a..b`,
                            // `n downTo 0`) counts in `Int`s.
                            let over_range = named_children(node).any(|part| {
                                part.kind() == "range_expression"
                                    || (part.kind() == "infix_expression"
                                        && named_children(part).any(|word| {
                                            matches!(text(word, self.source), "until" | "downTo")
                                        }))
                            });
                            for name in named_children(child).filter(|n| n.kind() == "identifier") {
                                let name = text(name, self.source).to_owned();
                                self.declare(
                                    &name,
                                    Var {
                                        ty: if over_range && child.kind() == "variable_declaration"
                                        {
                                            Ty::Prim(Prim::Int)
                                        } else {
                                            Ty::Unknown
                                        },
                                        local: false,
                                        constant: None,
                                    },
                                );
                            }
                        }
                        "block" => self.kotlin_node(child, Context::Statement),
                        "range_expression" => {
                            for bound in named_children(child) {
                                self.kotlin_node(bound, Context::Value);
                            }
                        }
                        "infix_expression" => {
                            // `0 until n`, `n downTo 0 step 2`
                            for part in named_children(child).filter(|part| {
                                part.kind() != "identifier"
                                    || !matches!(
                                        text(*part, self.source),
                                        "until" | "downTo" | "step"
                                    )
                            }) {
                                self.kotlin_node(part, Context::Value);
                            }
                        }
                        _ => self.kotlin_statement(child),
                    }
                }
                self.scopes.pop();
            }
            "when_expression" => {
                let subject = named_children(node).find(|child| child.kind() == "when_subject");
                if let Some(subject) = subject {
                    self.kotlin_children(subject, Context::Value);
                }
                for entry in named_children(node).filter(|child| child.kind() == "when_entry") {
                    for part in named_children(entry) {
                        let is_condition = entry
                            .children_by_field_name("condition", &mut entry.walk())
                            .any(|condition| condition == part);
                        let is_guard = entry.child_by_field_name("guard") == Some(part);
                        if is_condition && subject.is_none() {
                            self.kotlin_node(part, Context::Condition);
                        } else if is_condition || is_guard {
                            // A value the subject is compared with: the
                            // comparison is the compiler's, with no source to
                            // negate.
                            if is_guard {
                                self.kotlin_node(part, Context::Condition);
                            } else {
                                self.kotlin_children_values(part);
                            }
                        } else if context == Context::Statement {
                            self.kotlin_statement(part);
                        } else {
                            self.kotlin_node(part, context);
                        }
                    }
                }
            }
            "parenthesized_expression" => {
                if let Some(inner) = named_children(node).next() {
                    self.kotlin_node(inner, context);
                }
            }
            "binary_expression" => self.kotlin_binary(node, context),
            "infix_expression" => self.kotlin_infix(node, context),
            "unary_expression" => self.kotlin_unary(node, context),
            "assignment" => self.kotlin_assignment(node, context),
            "is_expression" => {
                if context == Context::Condition {
                    self.negate(node);
                }
                self.kotlin_children(node, Context::Value);
            }
            // `x in names` branched on is a `contains` call's jump; `x in
            // a..b` is two comparisons the source does not write.
            "in_expression"
                if context == Context::Condition
                    && named_children(node).last().is_some_and(|range| {
                        range.kind() != "range_expression"
                            && !matches!(
                                range
                                    .child_by_field_name("operator")
                                    .map(|operator| text(operator, self.source)),
                                Some("until" | "downTo" | "..<")
                            )
                            && !text(range, self.source).contains(" until ")
                            && !text(range, self.source).contains(" downTo ")
                    }) =>
            {
                self.negate(node);
                self.kotlin_children(node, Context::Value);
            }
            "call_expression" => {
                self.note_kotlin_logging(node);
                if context == Context::Condition || self.kotlin_negated_chain(node) {
                    self.negate(node);
                }
                if let Some(condition) = self.kotlin_checked_compound(node) {
                    self.negate(condition);
                }
                for child in named_children(node) {
                    self.kotlin_node(child, Context::Value);
                }
            }
            "identifier" => {
                let name = text(node, self.source);
                if context == Context::Condition
                    && !matches!(name, "true" | "false" | "null")
                    && self.kotlin_constant_value(node).is_none()
                {
                    self.negate(node);
                }
            }
            "navigation_expression" => {
                let safe = children(node).any(|child| child.kind() == "?.");
                if (context == Context::Condition && !safe) || self.kotlin_negated_chain(node) {
                    self.negate(node);
                }
                if let Some(receiver) = named_children(node).next() {
                    self.kotlin_node(receiver, Context::Value);
                }
            }
            "index_expression" | "as_expression" | "this_expression" => {
                if context == Context::Condition || self.kotlin_negated_chain(node) {
                    self.negate(node);
                }
                self.kotlin_children(node, Context::Value);
            }
            "value_argument"
            | "value_arguments"
            | "annotated_lambda"
            | "string_literal"
            | "interpolation"
            | "multiline_string_literal"
            | "collection_literal"
            | "spread_expression"
            | "range_expression"
            | "callable_reference"
            | "throw_expression"
            | "try_expression"
            | "catch_block"
            | "finally_block"
            | "labeled_expression"
            | "annotated_expression"
            | "in_expression"
            | "jump_expression"
            | "statement" => {
                for child in named_children(node) {
                    let inner = if matches!(
                        node.kind(),
                        "try_expression" | "catch_block" | "finally_block"
                    ) && child.kind() == "block"
                    {
                        Context::Statement
                    } else {
                        Context::Value
                    };
                    self.kotlin_node(child, inner);
                }
            }
            _ => self.kotlin_children(node, Context::Value),
        }
    }

    fn kotlin_children_values(&mut self, node: Node) {
        match node.kind() {
            "range_test" | "type_test" => {
                for child in named_children(node) {
                    self.kotlin_node(child, Context::Value);
                }
            }
            _ => self.kotlin_node(node, Context::Value),
        }
    }

    fn note_kotlin_logging(&mut self, call: Node) {
        let Some(callee) = named_children(call).next() else {
            return;
        };
        if callee.kind() != "navigation_expression" {
            return;
        }
        let Some(receiver) = named_children(callee).next() else {
            return;
        };
        let receiver = text(receiver, self.source);
        if self.loggers.contains(receiver)
            || matches!(receiver, "LoggerFactory" | "LogManager" | "LogFactory")
        {
            let line = named_children(callee)
                .last()
                .map_or(self.line(call.start_byte()), |name| {
                    self.line(name.start_byte())
                });
            self.logging_lines.insert(line);
        }
    }

    /// `require(a && b)`, `check(a || b)`: the inlined `if (!value) throw`
    /// is a jump of its own on the whole condition, besides its operands'.
    /// A single comparison's is the same mutant as the comparison's own.
    fn kotlin_checked_compound<'t>(&self, call: Node<'t>) -> Option<Node<'t>> {
        let callee = named_children(call).next()?;
        if !matches!(text(callee, self.source), "require" | "check") {
            return None;
        }
        let arguments = named_children(call).find(|child| child.kind() == "value_arguments")?;
        let mut values = named_children(arguments);
        let argument = values.next()?;
        if values.next().is_some() {
            return None;
        }
        let value = strip_parens(named_children(argument).last()?);
        (value.kind() == "binary_expression"
            && matches!(binary_operator(value, self.source).as_str(), "&&" | "||"))
        .then_some(value)
    }

    fn kotlin_binary(&mut self, node: Node, context: Context) {
        let operator = binary_operator(node, self.source);
        let (Some(left), Some(right)) = (
            node.child_by_field_name("left"),
            node.child_by_field_name("right"),
        ) else {
            self.kotlin_children(node, Context::Value);
            return;
        };
        match operator.as_str() {
            "&&" | "||" => {
                self.kotlin_node(left, Context::Condition);
                self.kotlin_node(right, Context::Condition);
            }
            "<" | "<=" | ">" | ">=" | "==" | "!=" | "===" | "!==" => {
                let constant = self.kotlin_constant_value(node).is_some();
                if !constant {
                    if let Some((helper, to)) = compare_helper(&operator) {
                        self.boundary(node, left, right, &operator, helper, to);
                    }
                    let jumps = match operator.as_str() {
                        // `a == b` of references is `Intrinsics.areEqual`,
                        // a value; it is a jump where it is a condition.
                        "==" => {
                            context == Context::Condition
                                || self.kotlin_primitive_equality(left, right)
                        }
                        _ => true,
                    };
                    let null_check = matches!(operator.as_str(), "==" | "!=")
                        && [left, right]
                            .iter()
                            .any(|side| text(*side, self.source) == "null");
                    if jumps || null_check {
                        self.negate(node);
                    }
                }
                self.kotlin_node(left, Context::Value);
                self.kotlin_node(right, Context::Value);
            }
            "?:" => {
                // The compiler's null check has no source to negate.
                self.kotlin_node(left, Context::Value);
                self.kotlin_node(right, context);
            }
            _ => {
                if let Some((to, helper)) = math_of(&operator)
                    && self.kotlin_constant_value(node).is_none()
                    && !self.kotlin_not_arithmetic(node, left, right, &operator)
                {
                    self.arith(node, left, right, &operator, to, helper);
                }
                if context == Context::Condition {
                    self.negate(node);
                }
                self.kotlin_node(left, Context::Value);
                self.kotlin_node(right, Context::Value);
            }
        }
    }

    /// Whether `+` and the like here is an operator function rather than
    /// arithmetic: a string's or a collection's `plus`, a `BigDecimal`'s.
    fn kotlin_not_arithmetic(&self, node: Node, left: Node, right: Node, operator: &str) -> bool {
        let left_ty = self.kotlin_type_of(left);
        let right_ty = self.kotlin_type_of(right);
        let _ = node;
        if operator == "+" && (left_ty == Ty::Str || matches!(left.kind(), "string_literal")) {
            return true;
        }
        // A side known to be a reference: Kotlin's operators are functions
        // any type may declare (`a + b` of two styles), which PIT does not
        // mutate. Where neither side's type is known the mutant is listed;
        // one whose type has no replacement operator is unviable.
        let reference = |ty: &Ty| matches!(ty, Ty::Str | Ty::Object(_));
        reference(&left_ty) || reference(&right_ty)
    }

    fn kotlin_primitive_equality(&self, left: Node, right: Node) -> bool {
        let literal = |node: Node| {
            matches!(
                node.kind(),
                "number_literal" | "float_literal" | "character_literal"
            ) || matches!(text(node, self.source), "true" | "false")
        };
        if literal(left) || literal(right) {
            return true;
        }
        matches!(
            (self.kotlin_type_of(left), self.kotlin_type_of(right)),
            (Ty::Prim(_), Ty::Prim(_))
        )
    }

    fn kotlin_infix(&mut self, node: Node, context: Context) {
        let parts = named_children(node).collect::<Vec<_>>();
        if parts.len() == 3 && parts[1].kind() == "identifier" {
            let operator = text(parts[1], self.source).to_owned();
            if matches!(
                operator.as_str(),
                "and" | "or" | "xor" | "shl" | "shr" | "ushr"
            ) && let Some((to, helper)) = math_of(&operator)
                && self.kotlin_constant_value(node).is_none()
            {
                let left_ty = self.kotlin_type_of(parts[0]);
                if !matches!(left_ty, Ty::Object(_) | Ty::Str) {
                    self.arith(node, parts[0], parts[2], &operator, to, helper);
                }
            }
            if context == Context::Condition {
                self.negate(node);
            }
            self.kotlin_node(parts[0], Context::Value);
            self.kotlin_node(parts[2], Context::Value);
            return;
        }
        if context == Context::Condition {
            self.negate(node);
        }
        self.kotlin_children(node, Context::Value);
    }

    /// A call, navigation or index chain the grammar leads with a `!`
    /// (`!name.isEmpty()`): Kotlin negates the whole chain, one jump to
    /// negate wherever the chain stands, the outermost chain only.
    fn kotlin_negated_chain(&self, node: Node) -> bool {
        if kotlin_chain_receiver(node) {
            return false;
        }
        let mut lead = node;
        while matches!(
            lead.kind(),
            "call_expression" | "navigation_expression" | "index_expression"
        ) {
            match named_children(lead).next() {
                Some(first) => lead = first,
                None => return false,
            }
        }
        lead.id() != node.id()
            && lead.kind() == "unary_expression"
            && lead
                .child_by_field_name("operator")
                .is_some_and(|op| text(op, self.source) == "!")
    }

    fn kotlin_unary(&mut self, node: Node, context: Context) {
        let operator = node
            .child_by_field_name("operator")
            .map(|op| text(op, self.source).to_owned())
            .unwrap_or_default();
        let operand = node
            .child_by_field_name("argument")
            .expect("the grammar gives a unary expression its operand");
        match operator.as_str() {
            // `!a.b()` is `!(a.b())`, which the grammar reads as `(!a).b()`:
            // the chain holds the jump, and `a` is its receiver.
            "!" if kotlin_chain_receiver(node) => self.kotlin_node(operand, Context::Value),
            "!" => self.kotlin_node(operand, Context::Condition),
            "-" => {
                if self.kotlin_constant_value(node).is_none()
                    && !matches!(self.kotlin_type_of(operand), Ty::Object(_))
                {
                    self.invert(node, operand);
                }
                if context == Context::Condition {
                    self.negate(node);
                }
                self.kotlin_node(operand, Context::Value);
            }
            "++" | "--" => {
                let prefix = text(node, self.source).starts_with(&operator);
                let ty = self.kotlin_type_of(operand);
                if ty.prim().is_some_and(Prim::numeric) || ty == Ty::Unknown {
                    self.step(
                        node,
                        operand,
                        prefix,
                        operator == "++",
                        context == Context::Statement,
                    );
                }
                self.kotlin_node(operand, Context::Value);
            }
            "!!" => self.kotlin_node(operand, context),
            _ => self.kotlin_node(operand, Context::Value),
        }
    }

    fn kotlin_assignment(&mut self, node: Node, _context: Context) {
        let operator = node
            .child_by_field_name("operator")
            .map(|op| text(op, self.source).to_owned())
            .unwrap_or_default();
        let left = node
            .child_by_field_name("left")
            .expect("the grammar gives an assignment its target");
        let right = node
            .child_by_field_name("right")
            .expect("the grammar gives an assignment its value");
        if operator != "=" {
            let ty = self.kotlin_type_of(left);
            if ty.prim().is_some_and(Prim::numeric) || ty == Ty::Unknown {
                // An assignment is a statement in Kotlin.
                self.java_compound(node, left, right, &operator, Context::Statement);
            }
        }
        self.kotlin_node(left, Context::Value);
        self.kotlin_node(right, Context::Value);
    }

    fn kotlin_loop_counter(&mut self, node: Node) {
        let condition = node
            .child_by_field_name("condition")
            .expect("the grammar gives a while loop its condition");
        let mut first = strip_parens(condition);
        while first.kind() == "binary_expression" {
            first = strip_parens(
                first
                    .child_by_field_name("left")
                    .expect("the grammar gives a binary expression its left operand"),
            );
        }
        if first.kind() != "identifier" {
            return;
        }
        let counter = text(first, self.source).to_owned();
        // The body follows the condition: a block, or one statement.
        let body = named_children(node)
            .last()
            .filter(|body| *body != condition);
        let last = match body {
            Some(body) if body.kind() == "block" => named_children(body)
                .filter(|child| !child.kind().ends_with("comment"))
                .last(),
            other => other,
        };
        let Some(last) = last else {
            return;
        };
        let target = match last.kind() {
            "unary_expression" => last.child_by_field_name("argument"),
            "assignment" => last.child_by_field_name("left"),
            _ => None,
        };
        if target.is_some_and(|target| text(target, self.source) == counter) {
            let range = (last.start_byte(), last.end_byte());
            self.mutants
                .retain(|mutant| !(mutant.mutator == Mutator::Increments && mutant.range == range));
        }
    }

    fn kotlin_return(&mut self, statement: Node, value: Node) {
        let method = self
            .method()
            .expect("a return is walked inside the method it returns from");
        let Some(returns) = method.returns.clone() else {
            return;
        };
        let not_null = method.not_null;
        let tail = self.kotlin_tail(value);
        let value = strip_parens(value);
        // `get() = throw E()` returns nothing: the throw is all there is.
        if text(value, self.source).starts_with("throw ") {
            return;
        }
        let range = (value.start_byte(), value.end_byte());
        // The compiler puts a function's return on the line its value ends.
        let lines = (
            self.line(statement.start_byte()),
            self.line(statement.end_byte()),
        );
        let mut replacements = Vec::new();
        match &returns {
            Ty::Void | Ty::Unknown => {}
            Ty::Prim(Prim::Boolean) => {
                if tail != Tail::One {
                    replacements.push(ReturnValue::True);
                }
                if tail != Tail::Zero {
                    replacements.push(ReturnValue::False);
                }
            }
            Ty::Boxed(Prim::Boolean) => {
                if tail != Tail::One {
                    replacements.push(ReturnValue::BoxedTrue);
                }
                if tail != Tail::Zero {
                    replacements.push(ReturnValue::BoxedFalse);
                }
            }
            Ty::Prim(prim) => {
                if tail != Tail::Zero {
                    replacements.push(ReturnValue::Zero(*prim));
                }
            }
            other => {
                if let Some(empty) = empty_value(other, self.language) {
                    if tail != Tail::Empty
                        && !(matches!(empty, ReturnValue::BoxedZero(_)) && tail == Tail::Zero)
                    {
                        replacements.push(empty);
                    }
                } else if tail != Tail::Null && !not_null {
                    replacements.push(ReturnValue::Null);
                }
            }
        }
        let witness = method.written_returns.clone();
        let method_name = method.name.clone();
        let probe = return_probe(value, None);
        let before = self.mutants.len();
        for replacement in replacements {
            self.push(
                replacement.mutator(),
                replacement.descriptor(),
                range,
                replacement.shown(self.language).to_owned(),
                None,
                Placement {
                    span: range,
                    holes: vec![range],
                    form: Form::Return(replacement, witness.clone()),
                    layer: 0,
                },
                method_name.clone(),
                lines,
            );
        }
        // One probe serves every return mutant of the value.
        if !ends_in_braced_branch(value)
            && let Some(first) = self.mutants.get_mut(before)
        {
            first.probe = Some(probe);
        }
    }

    fn kotlin_tail(&self, value: Node) -> Tail {
        let value = strip_parens(value);
        match value.kind() {
            "if_expression" => {
                let condition = value.child_by_field_name("condition");
                named_children(value)
                    .filter(|child| Some(*child) != condition)
                    .last()
                    .map_or(Tail::Other, |branch| self.kotlin_tail(branch))
            }
            "when_expression" => named_children(value)
                .filter(|child| child.kind() == "when_entry")
                .last()
                .and_then(|entry| named_children(entry).last())
                .map_or(Tail::Other, |branch| self.kotlin_tail(branch)),
            "block" | "statements" => named_children(value)
                .last()
                .map_or(Tail::Other, |last| self.kotlin_tail(last)),
            "binary_expression" => {
                let operator = binary_operator(value, self.source);
                match operator.as_str() {
                    "<" | "<=" | ">" | ">=" | "!=" | "===" | "!==" | "&&" | "||" => Tail::Zero,
                    "==" => {
                        let left = value
                            .child_by_field_name("left")
                            .expect("the grammar gives a binary expression its left operand");
                        let right = value
                            .child_by_field_name("right")
                            .expect("the grammar gives a binary expression its right operand");
                        if self.kotlin_primitive_equality(left, right)
                            || [left, right]
                                .iter()
                                .any(|side| text(*side, self.source) == "null")
                        {
                            Tail::Zero
                        } else {
                            Tail::Other
                        }
                    }
                    _ => self.kotlin_constant_tail(value),
                }
            }
            "unary_expression"
                if value
                    .child_by_field_name("operator")
                    .is_some_and(|op| text(op, self.source) == "!") =>
            {
                Tail::Zero
            }
            "identifier" if text(value, self.source) == "null" => Tail::Null,
            "string_literal" if text(value, self.source) == "\"\"" => Tail::Empty,
            // Kotlin's empty collections are its own objects, not the JDK's
            // that PIT's filter knows.
            _ => self.kotlin_constant_tail(value),
        }
    }

    fn kotlin_constant_tail(&self, value: Node) -> Tail {
        match self.kotlin_constant_value(value) {
            Some(Constant::Number(0.0)) => Tail::Zero,
            Some(Constant::Number(1.0)) => Tail::One,
            Some(Constant::Bool(false)) => Tail::Zero,
            Some(Constant::Bool(true)) => Tail::One,
            _ => Tail::Other,
        }
    }

    fn kotlin_constant_value(&self, node: Node) -> Option<Constant> {
        let node = strip_parens(node);
        match node.kind() {
            "number_literal" | "float_literal" => {
                number_literal(text(node, self.source)).map(|(value, _)| Constant::Number(value))
            }
            "character_literal" => Some(Constant::Number(1.0)),
            "string_literal"
                if !named_children(node).any(|child| child.kind() == "interpolation") =>
            {
                Some(Constant::Text)
            }
            "identifier" => match text(node, self.source) {
                "true" => Some(Constant::Bool(true)),
                "false" => Some(Constant::Bool(false)),
                name => self.lookup(name).and_then(|var| var.constant),
            },
            "unary_expression" => {
                let operator = text(
                    node.child_by_field_name("operator")
                        .expect("the grammar gives a unary expression its operator"),
                    self.source,
                );
                let operand = self.kotlin_constant_value(
                    node.child_by_field_name("argument")
                        .expect("the grammar gives a unary expression its operand"),
                )?;
                match (operator, operand) {
                    // kotlinc folds `+1` as it does `-1`.
                    ("+", Constant::Number(value)) => Some(Constant::Number(value)),
                    ("-", Constant::Number(value)) => Some(Constant::Number(-value)),
                    ("!", Constant::Bool(value)) => Some(Constant::Bool(!value)),
                    _ => None,
                }
            }
            "binary_expression" => {
                let operator = binary_operator(node, self.source);
                let left = self.kotlin_constant_value(
                    node.child_by_field_name("left")
                        .expect("the grammar gives a binary expression its left operand"),
                )?;
                let right = self.kotlin_constant_value(
                    node.child_by_field_name("right")
                        .expect("the grammar gives a binary expression its right operand"),
                )?;
                Some(match (left, right) {
                    (Constant::Number(a), Constant::Number(b)) => match operator.as_str() {
                        "+" => Constant::Number(a + b),
                        "-" => Constant::Number(a - b),
                        "*" => Constant::Number(a * b),
                        "/" if b != 0.0 => Constant::Number((a / b).trunc()),
                        "%" if b != 0.0 => Constant::Number(a % b),
                        "<" => Constant::Bool(a < b),
                        "<=" => Constant::Bool(a <= b),
                        ">" => Constant::Bool(a > b),
                        ">=" => Constant::Bool(a >= b),
                        "==" => Constant::Bool(a == b),
                        "!=" => Constant::Bool(a != b),
                        _ => return None,
                    },
                    (Constant::Bool(a), Constant::Bool(b)) => match operator.as_str() {
                        "&&" => Constant::Bool(a && b),
                        "||" => Constant::Bool(a || b),
                        "==" => Constant::Bool(a == b),
                        "!=" => Constant::Bool(a != b),
                        _ => return None,
                    },
                    (Constant::Text, Constant::Text) if operator == "+" => Constant::Text,
                    _ => return None,
                })
            }
            _ => None,
        }
    }

    fn kotlin_type_of(&self, node: Node) -> Ty {
        let node = strip_parens(node);
        match node.kind() {
            "number_literal" | "float_literal" => {
                let literal = text(node, self.source);
                let lower = literal.to_ascii_lowercase();
                // The grammar's integer literals have no `.` or exponent.
                if node.kind() == "float_literal" {
                    return if lower.ends_with('f') {
                        Ty::Prim(Prim::Float)
                    } else {
                        Ty::Prim(Prim::Double)
                    };
                }
                number_literal(literal).map_or(Ty::Unknown, |(_, prim)| Ty::Prim(prim))
            }
            "character_literal" => Ty::Prim(Prim::Char),
            "string_literal" | "multiline_string_literal" => Ty::Str,
            "identifier" => match text(node, self.source) {
                "true" | "false" => Ty::Prim(Prim::Boolean),
                "null" => Ty::Null,
                name => self.lookup(name).map_or(Ty::Unknown, |var| var.ty.clone()),
            },
            "is_expression" | "in_expression" => Ty::Prim(Prim::Boolean),
            "as_expression" => named_children(node)
                .last()
                .map_or(Ty::Unknown, |ty| kotlin_type(text(ty, self.source))),
            "if_expression" => {
                let condition = node.child_by_field_name("condition");
                let branches = named_children(node)
                    .filter(|child| Some(*child) != condition)
                    .map(|branch| self.kotlin_type_of(branch))
                    .collect::<Vec<_>>();
                match branches.as_slice() {
                    [a, b] if a == b => a.clone(),
                    [Ty::Prim(a), Ty::Prim(b)] if a.numeric() && b.numeric() => Ty::Unknown,
                    [a, Ty::Null] | [Ty::Null, a] => match a {
                        Ty::Prim(prim) => Ty::Boxed(*prim),
                        other => other.clone(),
                    },
                    [a, Ty::Unknown] | [Ty::Unknown, a] if a != &Ty::Void => a.clone(),
                    _ => Ty::Unknown,
                }
            }
            "when_expression" => {
                let types = named_children(node)
                    .filter(|child| child.kind() == "when_entry")
                    .filter_map(|entry| named_children(entry).last())
                    .map(|branch| self.kotlin_type_of(branch))
                    .collect::<Vec<_>>();
                let mut known = types
                    .into_iter()
                    .filter(|ty| *ty != Ty::Unknown)
                    .collect::<Vec<_>>();
                known.dedup();
                if known.len() == 1 {
                    known[0].clone()
                } else {
                    Ty::Unknown
                }
            }
            "block" | "statements" => named_children(node)
                .last()
                .map_or(Ty::Unknown, |last| self.kotlin_type_of(last)),
            "binary_expression" => {
                let operator = binary_operator(node, self.source);
                if matches!(
                    operator.as_str(),
                    "<" | "<=" | ">" | ">=" | "==" | "!=" | "===" | "!==" | "&&" | "||"
                ) {
                    return Ty::Prim(Prim::Boolean);
                }
                let left = node
                    .child_by_field_name("left")
                    .map_or(Ty::Unknown, |n| self.kotlin_type_of(n));
                let right = node
                    .child_by_field_name("right")
                    .map_or(Ty::Unknown, |n| self.kotlin_type_of(n));
                if operator == "?:" {
                    return match left {
                        Ty::Boxed(prim) => Ty::Prim(prim),
                        other if other != Ty::Unknown => other,
                        _ => right,
                    };
                }
                if operator == "+" && left == Ty::Str {
                    return Ty::Str;
                }
                match (left, right) {
                    (Ty::Prim(a), Ty::Prim(b)) if a.numeric() && b.numeric() => {
                        Ty::Prim(a.promote(b))
                    }
                    _ => Ty::Unknown,
                }
            }
            "infix_expression" => {
                let parts = named_children(node).collect::<Vec<_>>();
                if parts.len() == 3 {
                    match text(parts[1], self.source) {
                        "and" | "or" | "xor" | "shl" | "shr" | "ushr" => {
                            return self.kotlin_type_of(parts[0]);
                        }
                        "until" | "downTo" | "step" | "to" => {
                            return Ty::Object("Range".into());
                        }
                        _ => {}
                    }
                }
                Ty::Unknown
            }
            "unary_expression" => {
                let operator = node
                    .child_by_field_name("operator")
                    .map(|op| text(op, self.source))
                    .unwrap_or("");
                let operand = node
                    .child_by_field_name("argument")
                    .map_or(Ty::Unknown, |operand| self.kotlin_type_of(operand));
                match operator {
                    "!" => Ty::Prim(Prim::Boolean),
                    "!!" => match operand {
                        Ty::Boxed(prim) => Ty::Prim(prim),
                        other => other,
                    },
                    _ => match operand {
                        Ty::Prim(prim) if prim.numeric() => Ty::Prim(prim.promote(Prim::Int)),
                        other => other,
                    },
                }
            }
            "call_expression" => {
                let callee = named_children(node)
                    .next()
                    .expect("the grammar gives a call its callee");
                let name = match callee.kind() {
                    "identifier" => text(callee, self.source),
                    "navigation_expression" => named_children(callee)
                        .last()
                        .map_or("", |name| text(name, self.source)),
                    _ => "",
                };
                match name {
                    "listOf" | "mutableListOf" | "emptyList" | "arrayListOf" | "toList"
                    | "toMutableList" | "sorted" | "sortedBy" | "filter" | "map" | "mapNotNull"
                    | "flatMap" | "reversed" => Ty::Object("List".into()),
                    "setOf" | "mutableSetOf" | "emptySet" | "toSet" | "hashSetOf" => {
                        Ty::Object("Set".into())
                    }
                    "mapOf" | "mutableMapOf" | "emptyMap" | "toMap" | "hashMapOf" => {
                        Ty::Object("Map".into())
                    }
                    "toInt" | "roundToInt" | "coerceIn" | "compareTo" | "sumOf" => {
                        if name == "coerceIn" || name == "sumOf" {
                            Ty::Unknown
                        } else {
                            Ty::Prim(Prim::Int)
                        }
                    }
                    "toLong" | "roundToLong" => Ty::Prim(Prim::Long),
                    "toDouble" => Ty::Prim(Prim::Double),
                    "toFloat" => Ty::Prim(Prim::Float),
                    "toString" | "trim" | "substring" | "uppercase" | "lowercase" | "format"
                    | "joinToString" | "repeat" | "replace" | "buildString" | "trimIndent"
                    | "trimMargin" | "padStart" | "padEnd" | "removePrefix" | "removeSuffix" => {
                        Ty::Str
                    }
                    "isEmpty" | "isNotEmpty" | "contains" | "startsWith" | "endsWith"
                    | "equals" | "any" | "all" | "none" | "isBlank" | "isNotBlank" | "matches" => {
                        Ty::Prim(Prim::Boolean)
                    }
                    _ => {
                        if callee.kind() == "identifier"
                            && name.chars().next().is_some_and(char::is_uppercase)
                        {
                            return Ty::Object(name.to_owned());
                        }
                        self.index.return_type(name).cloned().unwrap_or(Ty::Unknown)
                    }
                }
            }
            "navigation_expression" => {
                let name = named_children(node)
                    .last()
                    .map_or("", |name| text(name, self.source));
                let safe = children(node).any(|child| child.kind() == "?.");
                let ty = match name {
                    "size" | "length" | "lastIndex" | "ordinal" => Ty::Prim(Prim::Int),
                    _ => {
                        let receiver = named_children(node).next();
                        if receiver.is_some_and(|receiver| text(receiver, self.source) == "this") {
                            self.lookup(name).map_or(Ty::Unknown, |var| var.ty.clone())
                        } else {
                            Ty::Unknown
                        }
                    }
                };
                match (ty, safe) {
                    (Ty::Prim(prim), true) => Ty::Boxed(prim),
                    (ty, _) => ty,
                }
            }
            "lambda_literal" | "callable_reference" | "anonymous_function" => {
                Ty::Object("Function".into())
            }
            _ => Ty::Unknown,
        }
    }
}

/// Collection functions that are `inline`: their lambdas are the caller's
/// code.
fn kotlin_collection_inline(name: &str) -> bool {
    matches!(
        name,
        "filter"
            | "filterNot"
            | "filterIndexed"
            | "filterNotNull"
            | "map"
            | "mapIndexed"
            | "mapNotNull"
            | "flatMap"
            | "forEach"
            | "forEachIndexed"
            | "any"
            | "all"
            | "none"
            | "count"
            | "first"
            | "firstOrNull"
            | "last"
            | "lastOrNull"
            | "find"
            | "findLast"
            | "single"
            | "singleOrNull"
            | "sumOf"
            | "maxOf"
            | "minOf"
            | "maxByOrNull"
            | "minByOrNull"
            | "maxBy"
            | "minBy"
            | "sortedBy"
            | "sortedByDescending"
            | "groupBy"
            | "associate"
            | "associateBy"
            | "associateWith"
            | "partition"
            | "fold"
            | "reduce"
            | "takeWhile"
            | "dropWhile"
            | "indexOfFirst"
            | "indexOfLast"
            | "let"
            | "run"
            | "also"
            | "apply"
            | "with"
            | "takeIf"
            | "takeUnless"
            | "repeat"
            | "use"
            | "synchronized"
            | "buildString"
            | "buildList"
            | "buildMap"
            | "getOrElse"
            | "getOrPut"
            | "require"
            | "check"
            | "requireNotNull"
            | "checkNotNull"
            | "onEach"
            | "mapTo"
            | "filterTo"
            | "flatMapTo"
            | "zip"
            | "distinctBy"
            | "chunked"
            | "windowed"
            | "measureTimeMillis"
            | "withLock"
            | "ifEmpty"
            | "orElse"
            | "runCatching"
            | "fold_"
            | "mapKeys"
            | "mapValues"
            | "filterKeys"
            | "filterValues"
            | "forEachLine"
            | "sumBy"
            | "count_"
    )
}

/// Whether a property declares a nullable type (`val name: String?`).
fn nullable_property(property: Node, source: &str) -> bool {
    named_children(property)
        .find(|child| child.kind() == "variable_declaration")
        .and_then(|variable| named_children(variable).find(|child| child.kind().ends_with("type")))
        .is_some_and(|ty| text(ty, source).trim_end().ends_with('?'))
}

/// Whether a function declares a nullable return type: the type between
/// its parameters and its body (a receiver's comes before the parameters).
fn kotlin_declared_nullable(node: Node, source: &str) -> bool {
    children(node)
        .skip_while(|child| child.kind() != "function_value_parameters")
        .take_while(|child| child.kind() != "function_body")
        .find(|child| {
            matches!(
                child.kind(),
                "nullable_type" | "user_type" | "function_type"
            )
        })
        .is_some_and(|ty| {
            ty.kind() == "nullable_type" || text(ty, source).trim_end().ends_with('?')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ids of a file's mutants, enumerated as the run lists them.
    fn ids(file: &str, source: &str) -> Vec<String> {
        let index = ProjectIndex::of([(file, source)]);
        crate::jvm_strong::file_mutants(file, source, &index)
            .expect("parses")
            .mutants
            .iter()
            .map(|mutant| crate::jvm_strong::mutant_id(file, mutant))
            .collect()
    }

    /// An override of `Any`'s `hashCode`, `equals` or `toString` returns
    /// what `Any` declares, whatever its expression body's type reads as
    /// here: PIT mutates its return by that type.
    #[test]
    fn an_override_of_any_s_members_returns_any_s_type() {
        let file = "a/B.kt";
        let source = "class B(val a: Int) {\n    override fun hashCode() = hash(a)\n    override fun equals(other: Any?) = same(other)\n    override fun toString() = describe(a)\n    fun hashCode(x: Int) = hash(x)\n    fun other() = hash(a)\n}\n";
        let listed = ids(file, source);
        let on = |line: u32| {
            listed
                .iter()
                .filter(|id| id.starts_with(&format!("{file}:{line}:")))
                .map(|id| {
                    id.rsplit_once(':')
                        .map_or("", |(rest, _)| rest)
                        .rsplit(':')
                        .next()
                        .unwrap_or("")
                        .to_owned()
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(on(2), ["PrimitiveReturns"], "{listed:?}");
        assert_eq!(
            on(3),
            ["BooleanFalseReturnVals", "BooleanTrueReturnVals"],
            "{listed:?}"
        );
        assert_eq!(on(4), ["EmptyObjectReturnVals"], "{listed:?}");
        // Not `Any`'s: of a type the source does not say.
        assert!(on(5).is_empty() && on(6).is_empty(), "{listed:?}");
    }

    /// The ids on `line`, each as `Mutator:descriptor`.
    fn on_line(listed: &[String], file: &str, line: u32) -> Vec<String> {
        listed
            .iter()
            .filter(|id| id.starts_with(&format!("{file}:{line}:")))
            .map(|id| {
                let parts = id.rsplitn(3, ':').collect::<Vec<_>>();
                format!("{}:{}", parts[1], parts[0])
            })
            .collect()
    }

    /// Java's forms past the parity fixtures': a `var`, a bounded type
    /// variable, varargs, a block lambda, a C-style array, a catch and a
    /// try-with-resources, conditions that are calls, patterns, fields
    /// and array elements, and constants javac folds.
    #[test]
    fn java_forms_list_the_mutants_pit_makes_of_them() {
        let file = "a/J.java";
        let source = r#"package a;

import java.util.List;
import java.util.Map;

public class J {
    static final int H = 0x1F;
    static final int B = 0b101;
    static final int O = 017;
    static final long SHIFTED = H >>> 2;
    static final int NOT = ~H;
    static final boolean PICK = true ? false : true;
    static final int CAST = (int) 2.7;
    static final double HALF = 0.5;
    boolean ready;
    boolean[] flags = new boolean[2];

    static <T extends Comparable<T>> T max(T a, T b) {
        return a.compareTo(b) > 0 ? a : b;
    }

    static int sum(int... xs) {
        int s = 0;
        for (int x : xs) {
            s += x;
        }
        return s;
    }

    int run(List<String> names, Object o, Map.Entry<String, Integer> e) throws Exception {
        var count = names.size();
        int seen[] = new int[3];
        Runnable r = () -> {
            seen[0]++;
        };
        r.run();
        if (names.isEmpty()) {
            return 0;
        }
        if (o instanceof String) {
            count++;
        }
        if (this.ready && flags[0]) {
            count--;
        }
        try (java.io.StringReader reader = new java.io.StringReader("x")) {
            return reader.read() + count + H * 2 + e.getKey().length();
        } catch (RuntimeException failed) {
            throw failed;
        }
    }
}
"#;
        let listed = ids(file, source);
        let sorted = |line: u32| {
            let mut found = on_line(&listed, file, line);
            found.sort();
            found
        };
        // Constants javac folds: nothing to mutate.
        for line in 7..=15 {
            assert!(sorted(line).is_empty(), "{line}: {listed:?}");
        }
        assert_eq!(
            sorted(19),
            [
                "ConditionalsBoundary:>=",
                "NegateConditionals:negated",
                "NullReturnVals:null"
            ]
        );
        assert_eq!(sorted(25), ["Math:-="]);
        assert_eq!(sorted(27), ["PrimitiveReturns:0"]);
        assert_eq!(sorted(34), ["Math:--"]);
        assert_eq!(sorted(36), ["VoidMethodCall:removed"]);
        for line in [37, 40] {
            assert_eq!(sorted(line), ["NegateConditionals:negated"], "{line}");
        }
        assert_eq!(sorted(41), ["Increments:--"]);
        assert_eq!(
            sorted(43),
            ["NegateConditionals:negated", "NegateConditionals:negated"]
        );
        // The try-with-resources body's return is the compiler's, and
        // `H * 2` is folded: the three additions alone.
        assert_eq!(sorted(47), ["Math:-", "Math:-", "Math:-"]);
    }

    /// Kotlin's forms past the parity fixture's: an `init` block, a
    /// secondary constructor, an object expression, a lambda that
    /// destructures, a Boolean property set, a logger's line, a float, a
    /// `when` and an `if` as values, ranges, infix shifts, collections a
    /// call makes and a `this.` member.
    #[test]
    fn kotlin_forms_list_the_mutants_pit_makes_of_them() {
        let file = "a/K.kt";
        let source = r#"package a

import org.slf4j.LoggerFactory

class K(var count: Int) {
    private val logger = LoggerFactory.getLogger(K::class.java)
    var isOpen = false
    val ratio = 1.5f

    init {
        count = count + 1
    }

    constructor(text: String) : this(text.length * 2) {
        count = count - 1
    }

    fun pairs(map: Map<String, Int>): Int {
        logger.info("pairs " + (count + 1))
        isOpen = true
        val run = object : Runnable {
            override fun run() {
                count = count * 3
            }
        }
        run.run()
        val sizes = map.entries.map { (key, value) -> key.length + value }
        val small = if (count > 2) 1 else 0
        val kind = when (count) {
            1 -> 10
            else -> 20
        }
        val shifted = count shl 2
        val inside = count in 1..small
        val names = setOf("a") + mapOf("b" to 1).keys
        return sizes.size + small + kind + shifted + this.count + names.size + (if (inside) 1 else 0)
    }
}
"#;
        let listed = ids(file, source);
        let sorted = |line: u32| {
            let mut found = on_line(&listed, file, line);
            found.sort();
            found
        };
        assert_eq!(sorted(11), ["Math:-"]);
        assert_eq!(sorted(15), ["Math:+"]);
        // A logging line: nothing.
        assert!(sorted(19).is_empty(), "{listed:?}");
        assert_eq!(sorted(23), ["Math:/"]);
        assert_eq!(sorted(14), ["Math:/"]);
        // A call on an object whose type the source does not name: not
        // known to return nothing, so not listed.
        assert!(sorted(26).is_empty(), "{listed:?}");
        assert_eq!(sorted(27), ["Math:-"]);
        assert_eq!(
            sorted(28),
            ["ConditionalsBoundary:>=", "NegateConditionals:negated"]
        );
        assert_eq!(sorted(33), ["Math:shr"]);
        assert_eq!(sorted(36).len(), 8, "{listed:?}");
    }

    /// A body that is a `throw` (`get() = throw E()`) returns nothing: no
    /// return mutant, and no value probe around a value of no type.
    #[test]
    fn a_kotlin_body_that_throws_returns_nothing() {
        let file = "a/T.kt";
        let source = "interface T {\n    var border: Boolean\n        get() = throw UnsupportedOperationException()\n        set(value) {}\n    fun size(): Int = throw IllegalStateException()\n    fun count(): Int = 1 + 2 * size()\n}\n";
        let listed = ids(file, source);
        assert!(on_line(&listed, file, 3).is_empty(), "{listed:?}");
        assert!(on_line(&listed, file, 5).is_empty(), "{listed:?}");
        assert!(!on_line(&listed, file, 6).is_empty(), "{listed:?}");
    }

    /// Every compound assignment's operator, arithmetic in each numeric
    /// type, constants javac folds through `!`, `^`, `==` and `?:`, a
    /// condition a constant decides, and the types of ternaries, unary
    /// operators, `this`, lambdas and a map entry's key.
    #[test]
    fn java_operators_constants_and_types() {
        let file = "a/M.java";
        let source = r#"package a;

import java.util.Map;
import java.util.function.Supplier;

public class M {
    static final boolean A = true;
    static final boolean B = !A ^ (A == false) != true;
    static final int TERNARY = A ? 1 : 2;
    static final boolean DEBUG = false;

    long run(long l, float f, double d, int i, byte b, Map.Entry<String, Integer> e) {
        i *= 2;
        i |= 1;
        i &= 7;
        i >>= 1;
        i <<= 1;
        i /= 3;
        i %= 5;
        i ^= 4;
        i >>>= 1;
        l = l * 3L;
        f = f - 1.5f;
        d = d / 2.0;
        int n = -b;
        boolean not = !A;
        Supplier<M> self = () -> this;
        Object pick = i > 1 ? null : "x";
        long mixed = i > 2 ? 1 : 2L;
        if (DEBUG) {
            i++;
        } else {
            i--;
        }
        if (A) {
            i += 2;
        }
        return l + (long) f + (long) d + n + e.getKey().length() + e.getValue();
    }

    boolean flag(int i) {
        return i > 0 ? true : false;
    }

    boolean constant() {
        return B;
    }
}
"#;
        let listed = ids(file, source);
        let sorted = |line: u32| {
            let mut found = on_line(&listed, file, line);
            found.sort();
            found
        };
        for line in 7..=10 {
            assert!(sorted(line).is_empty(), "{line}: {listed:?}");
        }
        let expected = [
            (13, "Math:/="),
            (14, "Math:&="),
            (15, "Math:|="),
            (16, "Math:<<="),
            (17, "Math:>>="),
            (18, "Math:*="),
            (19, "Math:*="),
            (20, "Math:&="),
            (21, "Math:<<="),
            (22, "Math:/"),
            (23, "Math:+"),
            (24, "Math:*"),
        ];
        for (line, mutant) in expected {
            assert_eq!(sorted(line), [mutant], "{line}: {listed:?}");
        }
        assert_eq!(sorted(25), ["InvertNegs:removed"]);
        // `if (DEBUG)` leaves its consequence out, `if (A)` its
        // alternative: only the branch that runs is mutated.
        assert_eq!(sorted(27), ["NullReturnVals:null"]);
        for line in [28, 29] {
            assert_eq!(
                sorted(line),
                ["ConditionalsBoundary:>=", "NegateConditionals:negated"]
            );
        }
        // `if (DEBUG)` leaves its consequence out, `if (A)` its
        // alternative: only the branch that runs is mutated.
        assert!(sorted(31).is_empty(), "{listed:?}");
        assert_eq!(sorted(33), ["Increments:++"]);
        assert_eq!(sorted(36), ["Increments:-="]);
        assert_eq!(sorted(38).len(), 6, "{listed:?}");
        // `? true : false` returns false where it does not return true.
        assert_eq!(
            sorted(42),
            [
                "BooleanTrueReturnVals:true",
                "ConditionalsBoundary:>=",
                "NegateConditionals:negated"
            ]
        );
        // `B` folds to true: only its replacement with false.
        assert_eq!(sorted(46), ["BooleanFalseReturnVals:false"]);
    }

    // Coverage of package B2: Kotlin's unary operators and compound
    // assignments, while-loop counters, returns and their tails, folded
    // constants, inferred types, collection inlines and declared nullability.

    /// The ids on `line` of `listed`, sorted.
    fn sorted_on(listed: &[String], file: &str, line: u32) -> Vec<String> {
        let mut found = on_line(listed, file, line);
        found.sort();
        found
    }

    /// `-x`, `x++` and `x += y` are arithmetic only where the operand is a
    /// number or of a type the source does not say: an operator function
    /// (`BigDecimal`'s `-`, `BigInteger`'s `++`, a `-` that returns a
    /// Boolean the code branches on) and a string's `+=` are calls, and
    /// `+x` changes nothing.
    #[test]
    fn kotlin_unary_and_compound_operators_follow_the_operand_s_type() {
        let file = "a/U.kt";
        let source = r#"package a

import java.math.BigDecimal
import java.math.BigInteger

class Flag {
    operator fun unaryMinus(): Boolean = true
}

class Counter(var count: Int)

fun neg(b: BigDecimal): BigDecimal {
    return -b
}

fun flagged(flag: Flag): Int {
    if (-flag) return 1
    return 2
}

fun steps(c: Counter, big: BigInteger, x: Int): String {
    var b = big
    b++
    c.count++
    c.count += 2
    var s = ""
    s += "x"
    val p = +x
    return s + b + p
}
"#;
        let listed = ids(file, source);
        let sorted = |line: u32| sorted_on(&listed, file, line);
        // `BigDecimal.negate()`, returned where the type is not null.
        assert!(sorted(13).is_empty(), "{listed:?}");
        // A Boolean `-flag` branched on: negated, not inverted.
        assert_eq!(
            sorted(17),
            ["NegateConditionals:negated", "PrimitiveReturns:0"]
        );
        assert!(sorted(23).is_empty(), "{listed:?}");
        // Another object's property: of a type the source does not say.
        assert_eq!(sorted(24), ["Math:--"]);
        assert_eq!(sorted(25), ["Math:-="]);
        assert!(sorted(27).is_empty(), "{listed:?}");
        assert!(sorted(28).is_empty(), "{listed:?}");
        assert_eq!(sorted(29), ["EmptyObjectReturnVals:\"\""]);
    }

    /// PIT's loop-counter filter keeps a `while` loop's last statement
    /// unmutated where it increments the local its condition reads first,
    /// the loop's body a block or one statement; it leaves every other
    /// increment of a local `int`, and a loop whose condition is a call or
    /// whose body is empty has no counter to keep.
    #[test]
    fn kotlin_while_loops_keep_their_counter_s_increment() {
        let file = "a/W.kt";
        let source = r#"package a

fun loops(n: Int, items: MutableList<Int>, running: () -> Boolean): Int {
    var k = 0
    k++
    var i = 0
    while (i < n) i++
    var j = 0
    var total = 0
    while (j < n) {
        total += j
        j += 1
    }
    while (k < 0) {
    }
    while (items.isNotEmpty()) {
        items.removeAt(0)
    }
    while (running()) {
    }
    return total + k
}
"#;
        let listed = ids(file, source);
        let sorted = |line: u32| sorted_on(&listed, file, line);
        assert_eq!(sorted(5), ["Increments:--"]);
        for line in [7, 10, 14] {
            assert_eq!(
                sorted(line),
                ["ConditionalsBoundary:<=", "NegateConditionals:negated"],
                "{line}: {listed:?}"
            );
        }
        assert_eq!(sorted(11), ["Math:-="]);
        assert!(sorted(12).is_empty(), "{listed:?}");
        for line in [16, 19] {
            assert_eq!(sorted(line), ["NegateConditionals:negated"], "{line}");
        }
        assert_eq!(sorted(21), ["Math:-", "PrimitiveReturns:0"]);
    }

    /// A Kotlin return is mutated by its type as PIT chooses, less what it
    /// already returns: a `Boolean?` ending in `true` or a comparison, `""`,
    /// a boxed `0`, `null`, a block's last value. A receiver's `?` is not the
    /// return type's, and a getter of a type the source does not say has
    /// none to mutate by.
    #[test]
    fn kotlin_returns_leave_out_what_the_value_already_is() {
        let file = "a/R.kt";
        let source = r#"package a

class Foo

interface Resettable {
    fun reset()
}

class Box {
    val owner
        get() = Foo()
}

fun positive(x: Int): Boolean? = x > 0
fun yes(): Boolean? = true
fun same(x: Boolean?): Boolean? = x
fun empty(): String = ""
fun echo(x: String): String = x
fun zero(): Int? = 0
fun boxed(x: Int): Int? = x
fun none(): Foo? = null
fun some(): Foo? = Foo()
fun no(): Boolean = false
fun pick(c: Boolean): Int = if (c) { 1 } else { 0 }
fun pickLast(c: Boolean): Int = if (c) { 0 } else { 1 }
fun owner(): Foo = Foo()
fun String?.blank(): Boolean = this == null
fun String.twice(): String = repeat(2)
"#;
        let listed = ids(file, source);
        let sorted = |line: u32| sorted_on(&listed, file, line);
        for line in [6, 10, 11, 17, 19, 21, 26] {
            assert!(sorted(line).is_empty(), "{line}: {listed:?}");
        }
        let expected: [(u32, &[&str]); 11] = [
            (
                14,
                &[
                    "BooleanTrueReturnVals:true",
                    "ConditionalsBoundary:>=",
                    "NegateConditionals:negated",
                ],
            ),
            (15, &["BooleanFalseReturnVals:false"]),
            (
                16,
                &["BooleanFalseReturnVals:false", "BooleanTrueReturnVals:true"],
            ),
            (18, &["EmptyObjectReturnVals:\"\""]),
            (20, &["EmptyObjectReturnVals:0"]),
            (22, &["NullReturnVals:null"]),
            (23, &["BooleanTrueReturnVals:true"]),
            (24, &["NegateConditionals:negated"]),
            (25, &["NegateConditionals:negated", "PrimitiveReturns:0"]),
            (
                27,
                &["BooleanTrueReturnVals:true", "NegateConditionals:negated"],
            ),
            (28, &["EmptyObjectReturnVals:\"\""]),
        ];
        for (line, mutants) in expected {
            assert_eq!(sorted(line), mutants, "{line}: {listed:?}");
        }
    }

    /// kotlinc folds constants (`6 / 2`, `+2 - 1`, `true && false`, `"a" +
    /// "b"`), so PIT has nothing to mutate in them and nothing in the branch
    /// a constant condition leaves out; a division by zero, an elvis and an
    /// operator a file declares (`!5`) are not folded.
    #[test]
    fn kotlin_constants_fold_as_kotlinc_folds_them() {
        let file = "a/F.kt";
        let source = r#"package a

operator fun Int.not(): Int = -this

operator fun Boolean.unaryPlus(): Boolean = this

class Folds {
    var n = 0

    fun arithmetic(x: Int): Int {
        val a = 6 + 2
        val b = 6 - 2
        val c = 6 * 2
        val d = 6 / 2
        val e = 6 / 0
        val f = 6 % 4
        val g = 6 % 0
        val h = (2 ?: 3) * x
        val i = +2 - 1
        val left = !5 * 2
        val right = 2 * !5
        return a + b + c + d + e + f + g + h + i + left + right
    }

    fun logic(flag: Boolean) {
        if (true && false) n += 1 else n -= 1
        if (false && true) n += 1 else n -= 1
        if (true && true) n += 1 else n -= 1
        if (true || false) n += 1 else n -= 1
        if (false || false) n += 1 else n -= 1
        if (false || true) n += 1 else n -= 1
        if (true == false) n += 1 else n -= 1
        if (true != false) n += 1 else n -= 1
        if (!false) n += 1 else n -= 1
        if (-1 < 0) n += 1 else n -= 1
        if (+1 > 0) n += 1 else n -= 1
        if (1 <= 2) n += 1 else n -= 1
        if (2 > 3) n += 1 else n -= 1
        if (2 >= 3) n += 1 else n -= 1
        if (1 == 2) n += 1 else n -= 1
        if (1 != 2) n += 1 else n -= 1
        val same = (true ?: false) == flag
        val text = "a" == "b"
        val joined = "a" + "b"
    }

    fun lifted(b: Boolean) = +b
}
"#;
        let listed = ids(file, source);
        let sorted = |line: u32| sorted_on(&listed, file, line);
        assert_eq!(sorted(3), ["InvertNegs:removed", "PrimitiveReturns:0"]);
        assert_eq!(
            sorted(5),
            ["BooleanFalseReturnVals:false", "BooleanTrueReturnVals:true"]
        );
        for line in [11, 12, 13, 14, 16, 19] {
            assert!(sorted(line).is_empty(), "{line}: {listed:?}");
        }
        for line in [15, 17] {
            assert_eq!(sorted(line), ["Math:*"], "{line}: {listed:?}");
        }
        for line in [18, 20, 21] {
            assert_eq!(sorted(line), ["Math:/"], "{line}: {listed:?}");
        }
        // Only the branch the constant condition takes: `n += 1` where it
        // is true, `n -= 1` where it is false.
        let taken = [
            (26, false),
            (27, false),
            (28, true),
            (29, true),
            (30, false),
            (31, true),
            (32, false),
            (33, true),
            (34, true),
            (35, true),
            (36, true),
            (37, true),
            (38, false),
            (39, false),
            (40, false),
            (41, true),
        ];
        for (line, condition) in taken {
            let branch = if condition { "Math:-=" } else { "Math:+=" };
            assert_eq!(sorted(line), [branch], "{line}: {listed:?}");
        }
        assert_eq!(sorted(42), ["NegateConditionals:negated"]);
        assert!(sorted(43).is_empty(), "{listed:?}");
        assert!(sorted(44).is_empty(), "{listed:?}");
        assert_eq!(
            sorted(47),
            ["BooleanFalseReturnVals:false", "BooleanTrueReturnVals:true"]
        );
    }

    /// An expression body's return type is the expression's: a literal's,
    /// a cast's, an `if`'s or a `when`'s branches' where they agree (boxed
    /// where one is `null`), an elvis's, a `!!`'s, a negation's, and a
    /// library call's that the walker knows; none where branches disagree.
    #[test]
    fn kotlin_expression_bodies_return_the_expression_s_type() {
        let file = "a/E.kt";
        let source = r#"package a

import java.math.BigDecimal
import kotlin.math.abs

fun ratio() = 1.5
fun letter() = 'a'
fun cast(x: Any) = x as String
fun mixed(c: Boolean, i: Int, l: Long) = if (c) i else l
fun boolOrInt(c: Boolean) = if (c) true else 1
fun intOrBool(c: Boolean) = if (c) 1 else true
fun maybe(c: Boolean) = if (c) 1 else null
fun maybeNot(c: Boolean) = if (c) null else 1
fun maybeText(c: Boolean) = if (c) "x" else null
fun known(c: Boolean, x: Int) = if (c) 1 else abs(x)
fun unit(c: Boolean, x: Any) = if (c) x as Unit else x.hashCode()
fun braced(c: Boolean) = if (c) { 1 } else { 2 }
fun kinds(c: Boolean) = when {
    c -> "a"
    else -> 2
}
fun orZero(x: Int?, y: Int) = x ?: y
fun orText(s: String?, d: String) = s ?: d
fun lookup(m: Map<String, Int>, y: Int) = m["a"] ?: y
fun sure(x: Int?) = x!!
fun sureText(s: String?) = s!!
fun negByte(b: Byte) = -b
fun negBig(b: BigDecimal) = -b
fun table() = mapOf("a" to 1)
fun asDouble(x: Int) = x.toDouble()
fun asFloat(x: Int) = x.toFloat()
fun asText(x: Int) = x.toString()
fun starts(s: String) = s.startsWith("a")
"#;
        let listed = ids(file, source);
        // The return mutants on `line`: what the type decides.
        let returns = |line: u32| {
            let mut found = on_line(&listed, file, line)
                .into_iter()
                .filter(|id| id.contains("Return"))
                .collect::<Vec<_>>();
            found.sort();
            found
        };
        let zero = ["PrimitiveReturns:0"];
        let text = ["EmptyObjectReturnVals:\"\""];
        let boxed_zero = ["EmptyObjectReturnVals:0"];
        for line in [6, 7, 15, 17, 22, 24, 25, 27, 30, 31] {
            assert_eq!(returns(line), zero, "{line}: {listed:?}");
        }
        for line in [8, 14, 23, 26, 32] {
            assert_eq!(returns(line), text, "{line}: {listed:?}");
        }
        for line in [12, 13] {
            assert_eq!(returns(line), boxed_zero, "{line}: {listed:?}");
        }
        for line in [9, 10, 11, 16, 18, 28] {
            assert!(returns(line).is_empty(), "{line}: {listed:?}");
        }
        assert_eq!(returns(29), ["EmptyObjectReturnVals:emptyMap"]);
        assert_eq!(
            returns(33),
            ["BooleanFalseReturnVals:false", "BooleanTrueReturnVals:true"]
        );
        // `-b` of a `Byte` is an `Int` negated; of a `BigDecimal`, a call.
        assert_eq!(
            on_line(&listed, file, 27)
                .into_iter()
                .filter(|id| id.starts_with("InvertNegs"))
                .count(),
            1
        );
        assert!(on_line(&listed, file, 28).is_empty(), "{listed:?}");
    }

    /// A range is an object (`(0 until n) + 5` is a list's `plus`); an
    /// infix call of another name, a call through a parenthesized value,
    /// `coerceIn` and `sumOf` are of no type the walker knows. A lambda kept
    /// as a value runs later, in a method of its own; one passed to a
    /// collection function kotlinc inlines is the enclosing method's code.
    #[test]
    fn kotlin_ranges_calls_and_lambdas_as_values() {
        let file = "a/L.kt";
        let source = r#"package a

fun ranges(n: Int, a: List<Int>, b: List<Int>, g: () -> Int, x: Int, xs: List<Int>) {
    val listed = (0 until n) + 5
    val commented = 0 /* from */ until n
    val zipped = a zip b
    val called = (g)()
    val low = x.coerceIn(0, 9)
    val total = xs.sumOf { it }
    val square = { y: Int -> y * y }
    val ref = ::ranges
    val big = xs.any { it > 2 }
}
"#;
        let index = ProjectIndex::of([(file, source)]);
        let mutants = crate::jvm_strong::file_mutants(file, source, &index)
            .expect("parses")
            .mutants;
        let on = |line: u32| {
            mutants
                .iter()
                .filter(|mutant| mutant.start.0 == line)
                .collect::<Vec<_>>()
        };
        for line in [4, 5, 6, 7, 8, 11] {
            assert!(on(line).is_empty(), "{line}: {:?}", on(line));
        }
        let square = on(10);
        assert_eq!(square.len(), 1, "{square:?}");
        assert_eq!(square[0].mutator, Mutator::Math);
        assert_eq!(square[0].method, "ranges$lambda");
        assert!(square[0].deferred.is_some());
        let mut inlined = on(12)
            .iter()
            .map(|mutant| (mutant.mutator, mutant.method.as_str(), mutant.deferred))
            .collect::<Vec<_>>();
        inlined.sort_by_key(|(mutator, _, _)| *mutator);
        assert_eq!(
            inlined,
            [
                (Mutator::ConditionalsBoundary, "ranges", None),
                (Mutator::NegateConditionals, "ranges", None),
            ]
        );
    }
}
