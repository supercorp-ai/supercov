//! Java and Kotlin mutants in a run: listed from the run's sources and linked
//! to the coverage points that decide whether a test reached them, and the
//! strong pass's plan for the JVM driver (`SupercovStrong.java`).
//!
//! The list is `jvm_mutants::enumerate`'s, in its order, for every Java and
//! Kotlin file the run measured. The strong pass enumerates each file again
//! to place the switches, and checks it lists the same ids at the same
//! places: a switch is addressed by its index, so a list that moved would
//! switch the wrong mutant.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::coverage_analysis::PointKind;
use crate::jvm_instrumenter::JvmLanguage;
use crate::jvm_mutants::{JvmMutant, ProjectIndex};
use crate::mutant_list::{CandidateMutant, MutantOperator};

/// Whether a run's file is Java or Kotlin, by its name.
pub fn language_of(file: &str) -> Option<JvmLanguage> {
    if file.ends_with(".java") {
        Some(JvmLanguage::Java)
    } else if file.ends_with(".kt") {
        Some(JvmLanguage::Kotlin)
    } else {
        None
    }
}

/// A value probe's slot in the probe array, what it records and the site
/// id its outcomes are reported under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValueSite {
    pub site: String,
    pub kind: crate::jvm_mutants::ProbeKind,
}

/// The outcome each bit of a value probe's slot stands for
/// (`Supercov.java`'s `w*` helpers write them).
pub fn value_outcomes(kind: crate::jvm_mutants::ProbeKind, bits: u32) -> Vec<String> {
    use crate::jvm_mutants::ProbeKind;
    let names: &[&str] = match kind {
        ProbeKind::Compare => &["lt", "eq", "gt", "unordered"],
        ProbeKind::Operation => &["same", "differs"],
        ProbeKind::Return => &[
            "false",
            "true",
            "zero",
            "nonzero",
            "null",
            "empty-string",
            "empty-list",
            "empty-set",
            "empty-map",
            "empty-optional",
            "boxed-zero",
            "boolean-true",
            "boolean-false",
            "other",
        ],
    };
    names
        .iter()
        .enumerate()
        .filter(|(bit, _)| bits & (1 << bit) != 0)
        .map(|(_, name)| (*name).to_owned())
        .collect()
}

/// The site id of a probe in `file`: a comparison's start (`file:L:C`), or
/// an operation's or a return's span and kind.
fn site_id(file: &str, source: &str, probe: &crate::jvm_mutants::ValueProbe) -> String {
    use crate::jvm_mutants::ProbeKind;
    let start = crate::jvm_mutants::position(source, probe.span.0);
    let end = crate::jvm_mutants::position(source, probe.span.1);
    match probe.kind {
        ProbeKind::Compare => format!("{file}:{}:{}", start.0, start.1),
        ProbeKind::Operation => format!(
            "{file}:{}:{}-{}:{}#operation",
            start.0, start.1, end.0, end.1
        ),
        ProbeKind::Return => format!("{file}:{}:{}-{}:{}#return", start.0, start.1, end.0, end.1),
    }
}

/// A file's value probes, given slots from `next_probe` on: the edits that
/// place them, each slot's site, and each listed mutant's links to them
/// (`compare` for a comparison, `probe` for the rest). A comparison's probe
/// decides its boundary mutant and tells whether its negation ran; a
/// return's serves every return mutant of the value.
pub fn probe_edits(
    file: &str,
    source: &str,
    mutants: &[JvmMutant],
    listed: &mut [CandidateMutant],
    next_probe: &mut u64,
) -> (
    Vec<crate::go_instrumenter::GoEdit>,
    BTreeMap<u64, ValueSite>,
) {
    use crate::jvm_mutants::ProbeKind;
    let mut edits = Vec::new();
    let mut sites = BTreeMap::new();
    for mutant in mutants {
        let Some(probe) = &mutant.probe else {
            continue;
        };
        *next_probe += 1;
        let slot = *next_probe;
        let site = site_id(file, source, probe);
        for (at, rank, text) in &probe.edits {
            edits.push(crate::go_instrumenter::GoEdit {
                at: *at,
                rank: *rank,
                text: text.replace('\u{2}', &slot.to_string()),
            });
        }
        for (other, link) in mutants.iter().zip(listed.iter_mut()) {
            if other.range != probe.span {
                continue;
            }
            match probe.kind {
                ProbeKind::Compare => {
                    if matches!(
                        other.mutator,
                        crate::jvm_mutants::Mutator::ConditionalsBoundary
                            | crate::jvm_mutants::Mutator::NegateConditionals
                    ) {
                        link.compare = Some(site.clone());
                    }
                }
                ProbeKind::Operation => {
                    if std::ptr::eq(other, mutant) {
                        link.probe = Some(site.clone());
                    }
                }
                ProbeKind::Return => {
                    if is_return(other.mutator) {
                        link.probe = Some(site.clone());
                    }
                }
            }
        }
        sites.insert(
            slot,
            ValueSite {
                site,
                kind: probe.kind,
            },
        );
    }
    (edits, sites)
}

/// One file's mutants, with the ids of its points.
pub struct FileMutants {
    pub mutants: Vec<JvmMutant>,
    pub listed: Vec<CandidateMutant>,
    pub points: BTreeSet<String>,
}

/// The index every file of a run is enumerated with: its Java and Kotlin
/// files' declarations.
pub fn index_of(root: &Path, files: &[String]) -> ProjectIndex {
    let sources = files
        .iter()
        .filter(|file| language_of(file).is_some())
        .filter_map(|file| {
            std::fs::read_to_string(root.join(file))
                .ok()
                .map(|source| (file.clone(), source))
        })
        .collect::<Vec<_>>();
    ProjectIndex::of(
        sources
            .iter()
            .map(|(file, source)| (file.as_str(), source.as_str())),
    )
}

/// One file's mutants, linked to its statement and function points.
pub fn file_mutants(file: &str, source: &str, index: &ProjectIndex) -> Result<FileMutants, String> {
    let language = language_of(file).ok_or_else(|| format!("{file} is not Java or Kotlin"))?;
    let mut next_probe = 0;
    let mut next_decision = 0;
    // Its obligations fail only where the source does not parse.
    let obligations = crate::jvm_instrumenter::build_jvm_obligations(
        file,
        source,
        language,
        &mut next_probe,
        &mut next_decision,
    )
    .map_err(|e| e.to_string())?;
    let tree = crate::jvm_instrumenter::parse(source, language)
        .expect("a source whose obligations were built parses");
    let mutants = crate::jvm_mutants::enumerate(source, &tree, language, index);
    let points = obligations
        .manifest
        .points
        .iter()
        .map(|point| point.id.clone())
        .collect::<BTreeSet<_>>();
    let listed = mutants
        .iter()
        .map(|mutant| candidate(mutant, &obligations.point_spans))
        .collect();
    Ok(FileMutants {
        mutants,
        listed,
        points,
    })
}

/// The innermost point of `kind` whose span holds `range`, with its span.
fn innermost(
    spans: &[(String, PointKind, usize, usize)],
    kind: PointKind,
    range: (usize, usize),
) -> Option<&(String, PointKind, usize, usize)> {
    spans
        .iter()
        .filter(|(_, point, start, end)| *point == kind && *start <= range.0 && range.1 <= *end)
        .min_by_key(|(_, _, start, end)| end - start)
}

pub(crate) fn candidate(
    mutant: &JvmMutant,
    spans: &[(String, PointKind, usize, usize)],
) -> CandidateMutant {
    // A point that holds the lambda the mutant is deferred in ran when the
    // lambda was made, not when it ran: such a mutant's reach is unknown,
    // and every test is its candidate, unless its probe says otherwise.
    let own = |kind: PointKind| {
        let (point, _, start, end) = innermost(spans, kind, mutant.range)?;
        match mutant.deferred {
            Some(lambda) if *start <= lambda.0 && lambda.1 <= *end => None,
            _ => Some(point.clone()),
        }
    };
    CandidateMutant {
        id: String::new(),
        mutator: mutant.mutator.name().to_owned(),
        replacement: mutant.descriptor.clone(),
        start: mutant.start,
        end: mutant.end,
        original: mutant.original.clone(),
        operator: mutant.operator.as_ref().map(|(text, at)| MutantOperator {
            text: text.clone(),
            at: *at,
        }),
        value_used: false,
        effects: false,
        statement: own(PointKind::Statement),
        function: own(PointKind::Function),
        decision: None,
        branch: None,
        chain: None,
        compare: None,
        probe: None,
        arrow: None,
        equivalent: None,
        untestable: None,
        is_static: false,
        unplaceable: None,
        reads: None,
        log_only: false,
    }
}

/// The Java and Kotlin files of `recorded` (each file's point ids, as the
/// run's manifest has them) with their mutants; and the files whose points
/// are not the ones the run recorded, which changed since.
pub(crate) fn list_mutants(
    root: &Path,
    recorded: &BTreeMap<String, BTreeSet<String>>,
) -> (BTreeMap<String, Vec<CandidateMutant>>, Vec<String>) {
    let files = recorded
        .keys()
        .filter(|file| language_of(file).is_some())
        .cloned()
        .collect::<Vec<_>>();
    let index = index_of(root, &files);
    let listed = crate::threads::parallel_map(&files, |file| {
        let source = std::fs::read_to_string(root.join(file)).ok()?;
        let mutants = file_mutants(file, &source, &index).ok()?;
        (Some(&mutants.points) == recorded.get(file)).then_some(mutants.listed)
    });
    let mut mutants = BTreeMap::new();
    let mut changed = Vec::new();
    for (file, listed) in files.into_iter().zip(listed) {
        match listed {
            Some(list) if list.is_empty() => {}
            Some(list) => {
                mutants.insert(file, list);
            }
            None => changed.push(file),
        }
    }
    (mutants, changed)
}

/// The id `mutants.json` gives a mutant of `file`.
pub fn mutant_id(file: &str, mutant: &JvmMutant) -> String {
    format!(
        "{file}:{}:{}-{}:{}:{}:{}",
        mutant.start.0,
        mutant.start.1,
        mutant.end.0,
        mutant.end.1,
        mutant.mutator.name(),
        mutant.descriptor
    )
}

// --- PIT's report --------------------------------------------------------------------

/// One mutant of PIT's `mutations.xml`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PitMutant {
    /// The source file, from the class's package and PIT's `sourceFile`.
    pub file: String,
    pub line: u32,
    pub mutator: String,
    /// What the description says, as `JvmMutant::key` says it.
    pub key: String,
    pub class: String,
    pub method: String,
    pub description: String,
    pub status: String,
    pub killing_tests: Vec<String>,
    /// The mutated instruction's index in its method: the order of a
    /// line's mutants, where kotlinc gives a whole condition one line.
    pub index: u32,
}

impl PitMutant {
    /// Whether PIT detected it: killed, or a timeout, memory or run error.
    pub fn detected(&self) -> bool {
        matches!(
            self.status.as_str(),
            "KILLED" | "TIMED_OUT" | "MEMORY_ERROR" | "RUN_ERROR"
        )
    }
}

fn xml_unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn xml_tag<'t>(block: &'t str, tag: &str) -> Option<&'t str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = block.find(&open)? + open.len();
    let end = block[start..].find(&close)? + start;
    Some(&block[start..end])
}

fn pit_key(mutator: &str, description: &str) -> String {
    let word = |phrase: &str| -> String {
        match phrase {
            "addition" => "+",
            "subtraction" => "-",
            "multiplication" => "*",
            "division" => "/",
            "modulus" => "%",
            "bitwise AND" | "AND" => "&",
            "bitwise OR" | "OR" => "|",
            "XOR" => "^",
            "Shift Left" => "<<",
            "Shift Right" => ">>",
            "Unsigned Shift Right" => ">>>",
            other => other,
        }
        .to_owned()
    };
    match mutator {
        "Math" => {
            let rest = description.strip_prefix("Replaced ").unwrap_or(description);
            let rest = ["integer ", "long ", "float ", "double "]
                .iter()
                .find_map(|prefix| rest.strip_prefix(prefix))
                .unwrap_or(rest);
            match rest.split_once(" with ") {
                Some((from, to)) => format!("{}>{}", word(from), word(to)),
                None => String::new(),
            }
        }
        "VoidMethodCall" => description.rsplit("::").next().unwrap_or("").to_owned(),
        "Increments" => description
            .strip_prefix("Changed increment from ")
            .and_then(|rest| rest.split(' ').next())
            .unwrap_or("")
            .to_owned(),
        m if m.ends_with("ReturnVals") || m == "PrimitiveReturns" => {
            let method = description.rsplit("::").next().unwrap_or("");
            method
                .trim_end_matches(|c: char| c.is_ascii_digit())
                .to_owned()
        }
        _ => String::new(),
    }
}

/// Reads PIT's `mutations.xml` (written with `outputFormats` XML; with
/// `fullMutationMatrix` every test that kills each mutant is listed).
pub fn parse_pit_report(text: &str) -> Vec<PitMutant> {
    let mut mutants = Vec::new();
    for block in text.split("<mutation ").skip(1) {
        let status = block
            .split_once("status='")
            .or_else(|| block.split_once("status=\""))
            .and_then(|(_, rest)| rest.split(['\'', '"']).next())
            .unwrap_or("")
            .to_owned();
        let get = |tag: &str| xml_tag(block, tag).map(xml_unescape).unwrap_or_default();
        let mutator = get("mutator");
        let mutator = mutator.rsplit('.').next().unwrap_or("");
        let mutator = mutator
            .strip_suffix("Mutator")
            .unwrap_or(mutator)
            .to_owned();
        let class = get("mutatedClass");
        let package = class
            .rsplit_once('.')
            .map(|(package, _)| package)
            .unwrap_or("");
        let source = get("sourceFile");
        let file = if package.is_empty() {
            source
        } else {
            format!("{}/{source}", package.replace('.', "/"))
        };
        let description = get("description");
        let key = pit_key(&mutator, &description);
        let killing_tests = get("killingTests")
            .split('|')
            .filter(|test| !test.is_empty())
            .map(str::to_owned)
            .collect();
        mutants.push(PitMutant {
            file,
            line: get("lineNumber").parse().unwrap_or(0),
            mutator,
            key,
            method: get("mutatedMethod"),
            class,
            description,
            status,
            killing_tests,
            index: get("index").parse().unwrap_or(0),
        });
    }
    mutants
}

/// Supercov's mutants and PIT's, joined: each pair, and those of either
/// side the other does not have.
#[derive(Debug, Default)]
pub struct PitJoin {
    /// (Supercov's id, index into PIT's list)
    pub pairs: Vec<(String, usize)>,
    pub only_supercov: Vec<String>,
    pub only_pit: Vec<usize>,
}

/// Joins `ours` (each file's mutants, by the file's path from the project
/// root) with PIT's by source file, mutator and what its description says,
/// on the line PIT names: the operator's line first, then any line of the
/// statement the instruction belongs to. PIT names a file by package and
/// name, so a file matches where its path ends with PIT's.
pub fn join_pit(ours: &BTreeMap<String, Vec<JvmMutant>>, pit: &[PitMutant]) -> PitJoin {
    let mut join = PitJoin::default();
    let mut taken = vec![false; pit.len()];
    let mut by_group = BTreeMap::<(String, String, String), Vec<usize>>::new();
    let files = ours.keys().cloned().collect::<Vec<_>>();
    for (at, mutant) in pit.iter().enumerate() {
        let file = files
            .iter()
            .find(|file| *file == &mutant.file || file.ends_with(&format!("/{}", mutant.file)))
            .cloned()
            .unwrap_or_else(|| mutant.file.clone());
        // Its own key and the names it holds: one at least.
        pit_keys(mutant).into_iter().for_each(|key| {
            by_group
                .entry((file.clone(), mutant.mutator.clone(), key))
                .or_default()
                .push(at);
        });
    }
    // A group's mutants in code order, ours in source order, so that where
    // several share the line PIT reports (kotlinc puts a condition's jumps
    // on one line) each takes its own counterpart, not the first.
    for group in by_group.values_mut() {
        group.sort_by_key(|at| {
            (
                pit[*at].class.clone(),
                pit[*at].method.clone(),
                pit[*at].index,
            )
        });
    }
    let mut unmatched = Vec::new();
    for (file, mutants) in ours {
        for mutant in mutants {
            let key = if mutant.method.starts_with("lambda$") && is_return(mutant.mutator) {
                mutant.method.clone()
            } else {
                mutant.key.clone()
            };
            unmatched.push((file.clone(), mutant, key));
        }
    }
    // In the order the compiler emits them: by where the code ends, inner
    // code first, so an operand's jump comes before the jump of the
    // condition around it (`require(a || b)`: `a`, `b`, then the
    // require's own).
    unmatched.sort_by_key(|(file, mutant, _)| {
        (file.clone(), mutant.end, std::cmp::Reverse(mutant.start))
    });
    // A line where PIT has more of a group than Supercov is a line kotlinc
    // gave other lines' code (a condition's jumps all on its last line):
    // its mutants are paired in code order by the statement pass, not line
    // by line.
    let mut ours_on_line = BTreeMap::<(String, &str, String, u32), usize>::new();
    for (file, mutant, key) in &unmatched {
        *ours_on_line
            .entry((
                file.clone(),
                mutant.mutator.name(),
                key.clone(),
                mutant.start.0,
            ))
            .or_default() += 1;
    }
    let mut crowded = vec![false; pit.len()];
    let mut crowded_lines = std::collections::BTreeSet::<(String, String, String, u32)>::new();
    for ((file, mutator, key), group) in &by_group {
        // A group holds one mutant at least, and so a line.
        let mut theirs_on_line = BTreeMap::<u32, Vec<usize>>::new();
        group.iter().for_each(|at| {
            theirs_on_line.entry(pit[*at].line).or_default().push(*at);
        });
        theirs_on_line.into_iter().for_each(|(line, theirs)| {
            let ours = ours_on_line
                .get(&(file.clone(), mutator.as_str(), key.clone(), line))
                .copied()
                .unwrap_or(0);
            if ours > 0 && theirs.len() > ours {
                crowded_lines.insert((file.clone(), mutator.clone(), key.clone(), line));
                theirs.iter().for_each(|at| crowded[*at] = true);
            }
        });
    }
    // Three passes, each taking what the one before left: the exact line,
    // then any line of the mutant's statement, then (for a return) any line
    // of its method.
    for pass in 0..4 {
        let mut left = Vec::new();
        for (file, mutant, key) in unmatched {
            let group =
                by_group.get(&(file.clone(), mutant.mutator.name().to_owned(), key.clone()));
            // A Kotlin mutant of a crowded line has its counterpart there,
            // for the statement pass: not a lone one of a line before (a
            // `?.` of its own).
            let own_line_crowded = file.ends_with(".kt")
                && crowded_lines.contains(&(
                    file.clone(),
                    mutant.mutator.name().to_owned(),
                    key.clone(),
                    mutant.start.0,
                ));
            let fits = |at: usize| {
                !taken[at]
                    && match pass {
                        0 => pit[at].line == mutant.start.0 && !crowded[at],
                        // javac gives an operator on a continuation line
                        // the line of the operand before it, or of the
                        // statement.
                        1 => {
                            !crowded[at]
                                && !own_line_crowded
                                && (mutant.lines.0.saturating_sub(3)
                                    ..=mutant.lines.1.max(mutant.end.0))
                                    .contains(&pit[at].line)
                        }
                        // kotlinc puts a condition's jumps on its last
                        // line: the statement's every line.
                        2 => (mutant.lines.0..=mutant.statement_end).contains(&pit[at].line),
                        _ => is_return(mutant.mutator),
                    }
            };
            // In the statement pass a crowded line's mutants come first: they
            // are the ones the line pass left for it, where the others on
            // the statement's lines are code of their own (a `?.`).
            let found = group.and_then(|group| {
                let mut fitting = group.iter().copied().filter(|at| fits(*at));
                if pass == 2 {
                    let fitting = fitting.collect::<Vec<_>>();
                    fitting
                        .iter()
                        .copied()
                        .find(|at| crowded[*at])
                        .or_else(|| fitting.first().copied())
                } else {
                    fitting.next()
                }
            });
            match found {
                Some(at) => {
                    taken[at] = true;
                    join.pairs.push((mutant_id(&file, mutant), at));
                }
                None => left.push((file, mutant, key)),
            }
        }
        unmatched = left;
    }
    join.only_supercov = unmatched
        .into_iter()
        .map(|(file, mutant, _)| mutant_id(&file, mutant))
        .collect();
    join.only_pit = (0..pit.len()).filter(|at| !taken[*at]).collect();
    join
}

/// The names a mutant of PIT's may go by in source. kotlinc names a call
/// to an `internal` function after its module (`emit$kotlinpoet`), one
/// with default arguments `emit$default`, a local function after the one
/// around it (`emit$possiblyEmitInitializer`), and a private member an
/// accessor reaches `access$emit`: a removed call is joined by each name
/// such a method's name holds.
fn pit_keys(mutant: &PitMutant) -> Vec<String> {
    let mut keys = vec![mutant.key.clone()];
    let named = mutant.mutator == "VoidMethodCall"
        || mutant.mutator.ends_with("ReturnVals")
        || mutant.mutator == "PrimitiveReturns";
    // A Java lambda's method (`lambda$f$3`) is joined by its own name.
    if named && mutant.key.contains('$') && mutant.file.ends_with(".kt") {
        keys.extend(
            mutant
                .key
                .split('$')
                .filter(|part| !matches!(*part, "" | "access" | "default" | "p"))
                .map(str::to_owned),
        );
    }
    keys
}

fn is_return(mutator: crate::jvm_mutants::Mutator) -> bool {
    use crate::jvm_mutants::Mutator;
    matches!(
        mutator,
        Mutator::PrimitiveReturns
            | Mutator::BooleanTrueReturnVals
            | Mutator::BooleanFalseReturnVals
            | Mutator::NullReturnVals
            | Mutator::EmptyObjectReturnVals
    )
}

/// Whether a line calls a standard-library function that kotlinc inlines
/// with the lambda or reference given to it (`forEach`, `filter`, `use`,
/// `repeat`, ...): its loop, its counter, its conditions and its `finally`
/// are code the compiler writes at that line.
fn inlines_a_lambda(line: &str) -> bool {
    const INLINED: [&str; 30] = [
        "forEach",
        "filter",
        "filterNot",
        "filterKeys",
        "map",
        "mapNotNull",
        "mapValues",
        "all",
        "any",
        "none",
        "count",
        "find",
        "first",
        "sumBy",
        "sumOf",
        "maxOf",
        "minOf",
        "repeat",
        "use",
        "replaceFirstChar",
        "joinToString",
        "associate",
        "groupBy",
        "takeWhile",
        "withDefault",
        "buildString",
        "flatMap",
        "mapIndexed",
        "forEachIndexed",
        "indexOfFirst",
    ];
    INLINED.iter().any(|name| {
        line.match_indices(name).any(|(at, _)| {
            let before = line[..at].chars().next_back();
            let after = line[at + name.len()..].trim_start();
            !before.is_some_and(|c| c.is_alphanumeric() || c == '_')
                && (after.starts_with('{')
                    || after.starts_with('(') && (after.contains("::") || line.contains('{')))
        })
    })
}

/// What PIT made that no source can say, by the compiler construct it came
/// from: a disagreement explained rather than a missed mutant.
pub fn pit_only_category(mutant: &PitMutant, source: Option<&str>) -> &'static str {
    let line = source.and_then(|source| source.lines().nth(mutant.line.saturating_sub(1) as usize));
    let Some(line) = line else {
        return "inlined code (a line past the file's end)";
    };
    let kotlin = mutant.file.ends_with(".kt");
    let returns = mutant.mutator.ends_with("ReturnVals") || mutant.mutator == "PrimitiveReturns";
    if (mutant.method.starts_with("get") || mutant.method.starts_with("is"))
        && returns
        && kotlin
        && (line.contains("val ") || line.contains("var "))
    {
        return "a Kotlin property's generated getter";
    }
    if mutant.description.contains("SpreadBuilder") {
        return "a Kotlin spread operator's copying";
    }
    // A lambda or a function reference kotlinc compiles to a class of its
    // own (`FileSpec$emit$1::invoke`, `$suggestedTypeImports$1::get`).
    if kotlin
        && returns
        && (mutant.method == "invoke" || mutant.method == "get")
        && mutant.class.contains('$')
    {
        return "a Kotlin lambda's or function reference's own return";
    }
    if mutant.description.contains("Intrinsics") {
        return "a Kotlin null check the compiler wrote";
    }
    if kotlin && (line.contains("?.") || line.contains("?:") || line.contains("!!")) {
        return "a Kotlin null-safe operator";
    }
    if kotlin && line.trim_start().starts_with("for ") {
        return "a Kotlin for loop's counter";
    }
    // `IntArray(n) { 1 }` is a loop filling the array, inlined.
    if kotlin && line.contains("Array(") && line.contains('{') {
        return "a Kotlin array constructor's loop";
    }
    let code = line.trim_start();
    let conditional = mutant.mutator.contains("Conditionals");
    if kotlin
        && ((conditional && (code.contains("when (") || code.ends_with("-> {")))
            || code.starts_with("is ")
            || code.starts_with("!is ")
            || (line.contains("->") && !line.contains('{')))
    {
        return "a `when` branch's comparison with its subject";
    }
    // A property assigned (`builder.name = value`, `x += list`) is a call to
    // its setter, or to an operator the standard library inlines
    // (`putAll`, `add`): a call the source writes as no call.
    if kotlin && mutant.mutator == "VoidMethodCall" {
        let called = mutant.description.rsplit("::").next().unwrap_or("");
        let called = called.strip_prefix("access$").unwrap_or(called);
        // `setOpen` sets `open`, or `isOpen`: a Boolean property's name.
        let sets_it = called
            .strip_prefix("set")
            .and_then(|name| name.split('$').next())
            .filter(|name| !name.is_empty())
            .is_some_and(|name| {
                let mut chars = name.chars();
                let lower = chars
                    .next()
                    .map(|first| first.to_lowercase().chain(chars).collect::<String>());
                [lower.unwrap_or_default(), format!("is{name}")]
                    .iter()
                    .any(|property| {
                        code.starts_with(&format!("{property} = "))
                            || code.contains(&format!(".{property} = "))
                    })
            });
        // `counts[i] = n` and `counts[i]--` call an operator `set`.
        let indexed = called == "set"
            && (code.contains("] = ") || code.contains("]--") || code.contains("]++"));
        let operator = indexed
            || (called.starts_with("putAll") || called.starts_with("add"))
                && (code.contains(" += ") || code.contains(" -= "));
        if sets_it || operator {
            return "a Kotlin property assignment's setter or operator call";
        }
    }
    if kotlin && inlines_a_lambda(line) {
        return "code a standard library function inlines with its lambda";
    }
    if kotlin && conditional {
        // `require(c)` is `if (!c) throw`: a jump of its own beside the
        // comparison's, the same mutant where `c` is one comparison; and
        // `requireNotNull(x)` is `if (x == null) throw`.
        if [
            "require(",
            "check(",
            "require (",
            "check (",
            "requireNotNull(",
            "checkNotNull(",
        ]
        .iter()
        .any(|call| line.contains(call))
        {
            return "a Kotlin require or check's own jump";
        }
        // `x in a..b` is `a <= x && x <= b`, comparisons the source does not
        // write.
        if line.contains(" in ")
            && ["..", "until", "downTo"]
                .iter()
                .any(|range| line.contains(range))
        {
            return "a Kotlin range check";
        }
        // `isNotEmpty()` and the like are inlined `!isEmpty()`: the negation
        // is a jump of their own beside the condition's.
        if [
            "isEmpty()",
            "isNotEmpty()",
            "isNullOrEmpty()",
            "isNotBlank()",
            "isNullOrBlank()",
            "ifEmpty(",
            "orEmpty()",
        ]
        .iter()
        .any(|call| line.contains(call))
        {
            return "an inlined standard library function's own condition";
        }
    }
    "other"
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fixtures of `tests/fixtures/pit-parity` and PIT 1.17.1's own
    /// mutants of them (`*.pit.json`: its DEFAULTS group, from the
    /// `mutations.xml` of Maven with javac 21 for the Java, Gradle with
    /// Kotlin 2.0.21 for the Kotlin, read by `scripts/pit-parity.py`),
    /// joined as `supercov runs <id> mutants pit` joins them: what each side
    /// has that the other does not.
    fn parity(language: &str, files: &[&str]) -> (Vec<String>, Vec<String>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/pit-parity")
            .join(language);
        let golden: Vec<serde_json::Value> = serde_json::from_str(
            &std::fs::read_to_string(root.with_extension("pit.json")).unwrap(),
        )
        .unwrap();
        let pit = golden
            .iter()
            .map(|row| PitMutant {
                file: row["file"].as_str().unwrap().into(),
                line: row["line"].as_u64().unwrap() as u32,
                mutator: row["mutator"].as_str().unwrap().into(),
                key: row["key"].as_str().unwrap().into(),
                class: String::new(),
                method: row["method"].as_str().unwrap_or("").into(),
                description: row["description"].as_str().unwrap_or("").into(),
                status: "SURVIVED".into(),
                killing_tests: Vec::new(),
                index: 0,
            })
            .collect::<Vec<_>>();
        let sources = files
            .iter()
            .map(|file| {
                (
                    file.to_string(),
                    std::fs::read_to_string(root.join(file)).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let index = ProjectIndex::of(sources.iter().map(|(f, s)| (f.as_str(), s.as_str())));
        let ours = sources
            .iter()
            .map(|(file, source)| {
                (
                    file.clone(),
                    file_mutants(file, source, &index).unwrap().mutants,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let join = join_pit(&ours, &pit);
        let only_pit = join
            .only_pit
            .iter()
            .map(|at| {
                format!(
                    "{}:{} {} {}",
                    pit[*at].file, pit[*at].line, pit[*at].mutator, pit[*at].description
                )
            })
            .collect();
        (only_pit, join.only_supercov)
    }

    #[test]
    fn java_mutants_are_pit_s() {
        let (only_pit, only_supercov) = parity("java", &["fixture/Ops.java", "fixture/More.java"]);
        // A lambda passed to `Stream.map` returns whatever its body does;
        // the body's type (`x + 1` of an unknown `x`) is not in the source.
        assert_eq!(
            only_pit,
            [
                "fixture/More.java:118 EmptyObjectReturnVals replaced Integer return value with 0 for fixture/More::lambda$lambdas$6"
            ],
        );
        assert_eq!(only_supercov, Vec::<String>::new());
    }

    /// A Kotlin mutant on a line where PIT has more of its kind than
    /// Supercov (kotlinc's own null check beside it) takes a mutant of that
    /// line, not the lone one of the line above, which is a `?.` of its own.
    #[test]
    fn a_kotlin_mutant_of_a_crowded_line_takes_that_line_s() {
        let file = "fixture/Crowd.kt";
        let source = "fun f(a: String?, b: Boolean, c: Boolean): Boolean {\n    val n = a?.length\n    return b && c\n}\n";
        let index = ProjectIndex::of([(file, source)]);
        let mutants = file_mutants(file, source, &index).unwrap().mutants;
        let ours = mutants
            .into_iter()
            .filter(|mutant| mutant.start.0 == 3)
            .take(1)
            .collect::<Vec<_>>();
        assert_eq!(ours.len(), 1);
        let mutant = &ours[0];
        let pit_at = |line: u32, index: u32, status: &str| PitMutant {
            file: file.into(),
            line,
            mutator: mutant.mutator.name().into(),
            key: mutant.key.clone(),
            class: "fixture.CrowdKt".into(),
            method: mutant.method.clone(),
            description: String::new(),
            status: status.into(),
            killing_tests: Vec::new(),
            index,
        };
        let pit = vec![
            pit_at(2, 0, "SURVIVED"),
            pit_at(3, 1, "KILLED"),
            pit_at(3, 2, "KILLED"),
        ];
        let join = join_pit(&BTreeMap::from([(file.to_owned(), ours)]), &pit);
        assert_eq!(join.pairs.len(), 1);
        assert_eq!(pit[join.pairs[0].1].line, 3);
    }

    /// Mutants PIT 1.17.1 made on public projects that Supercov once missed:
    /// an anonymous class's methods in a static initializer (its own class,
    /// which PIT's static-initializer filter leaves alone), `Arrays.setAll`
    /// removed, and a lambda returning a value of the caller's own type
    /// variable, erased to Object.
    /// `require(a && b)` inlines to a jump on the whole condition besides
    /// its operands': PIT negates it, and so does Supercov. A single
    /// comparison's is the comparison's own mutant, listed once.
    /// PIT's jumps of `require(a || b)` come in the order kotlinc emits
    /// them, the operands' then the require's own: each pairs with the
    /// mutant of the same code, not the one that starts at the same place.
    #[test]
    fn a_require_s_jumps_pair_in_the_order_the_compiler_emits_them() {
        let file = "fixture/Pair.kt";
        let source = "fun f(a: String?) {\n    require(a == null || a.length > 1) { \"m\" }\n}\n";
        let index = ProjectIndex::of([(file, source)]);
        let ours = file_mutants(file, source, &index)
            .unwrap()
            .mutants
            .into_iter()
            .filter(|mutant| mutant.mutator.name() == "NegateConditionals")
            .collect::<Vec<_>>();
        assert_eq!(ours.len(), 3);
        let pit = (0..3)
            .map(|index| PitMutant {
                file: file.into(),
                line: 2,
                mutator: "NegateConditionals".into(),
                key: String::new(),
                class: "fixture.PairKt".into(),
                method: "f".into(),
                description: String::new(),
                status: ["KILLED", "SURVIVED", "TIMED_OUT"][index as usize].into(),
                killing_tests: Vec::new(),
                index,
            })
            .collect::<Vec<_>>();
        let join = join_pit(&BTreeMap::from([(file.to_owned(), ours)]), &pit);
        let status = |id: &str| {
            join.pairs
                .iter()
                .find(|(ours, _)| ours.contains(id))
                .map(|(_, at)| pit[*at].status.clone())
        };
        assert_eq!(status(":2:13-2:22:").as_deref(), Some("KILLED"));
        assert_eq!(status(":2:26-2:38:").as_deref(), Some("SURVIVED"));
        assert_eq!(status(":2:13-2:38:").as_deref(), Some("TIMED_OUT"));
    }

    #[test]
    fn a_kotlin_require_of_a_compound_condition_negates_the_whole() {
        let file = "fixture/Check.kt";
        let source = "fun f(i: Int, n: Int) {\n    require(i >= 0 && i < n) { \"index\" }\n    check(i < n)\n}\n";
        let index = ProjectIndex::of([(file, source)]);
        let ids = file_mutants(file, source, &index)
            .unwrap()
            .mutants
            .iter()
            .map(|mutant| mutant_id(file, mutant))
            .collect::<Vec<_>>();
        let negated = |line: u32| {
            ids.iter()
                .filter(|id| {
                    id.starts_with(&format!("{file}:{line}:")) && id.contains("NegateConditionals")
                })
                .cloned()
                .collect::<Vec<_>>()
        };
        assert_eq!(
            negated(2),
            [
                format!("{file}:2:13-2:28:NegateConditionals:negated"),
                format!("{file}:2:13-2:19:NegateConditionals:negated"),
                format!("{file}:2:23-2:28:NegateConditionals:negated"),
            ],
            "{ids:#?}"
        );
        assert_eq!(negated(3).len(), 1, "{ids:#?}");
    }

    /// A run's Java and Kotlin files listed again: those whose points are
    /// the run's give their mutants (none for a file with none), and one
    /// that changed since, or cannot be read, is named as changed.
    #[test]
    fn a_run_s_files_are_listed_again_or_named_as_changed() {
        let root = std::env::temp_dir().join(format!("supercov-jvm-list-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        let write = |file: &str, source: &str| std::fs::write(root.join(file), source).unwrap();
        write(
            "src/A.java",
            "class A {\n  int f(int n) {\n    return n * 2;\n  }\n}\n",
        );
        write("src/B.java", "class B {\n}\n");
        write(
            "src/C.java",
            "class C {\n  int g() {\n    return 1 + 2;\n  }\n}\n",
        );
        let index = ProjectIndex::of([("src/A.java", "class A {}"), ("src/B.java", "class B {}")]);
        let points = |file: &str| {
            let source = std::fs::read_to_string(root.join(file)).unwrap();
            file_mutants(file, &source, &index).unwrap().points
        };
        let recorded = BTreeMap::from([
            ("src/A.java".to_owned(), points("src/A.java")),
            ("src/B.java".to_owned(), points("src/B.java")),
            (
                "src/C.java".to_owned(),
                BTreeSet::from(["stale".to_owned()]),
            ),
            ("src/Gone.java".to_owned(), BTreeSet::new()),
            ("src/notes.txt".to_owned(), BTreeSet::new()),
        ]);
        let (mutants, changed) = list_mutants(&root, &recorded);
        std::fs::remove_dir_all(&root).unwrap();
        assert_eq!(mutants.keys().collect::<Vec<_>>(), ["src/A.java"]);
        assert_eq!(changed, ["src/C.java", "src/Gone.java"]);
    }

    /// PIT's XML report, read as `mutants pit` reads it: a mutant's file by
    /// its class's package, its status, its killing tests, the key its
    /// description gives, escapes undone.
    #[test]
    fn pit_s_report_is_read_mutant_by_mutant() {
        let report = "<?xml version=\"1.0\"?>\n<mutations>\n<mutation detected='true' status='KILLED'><sourceFile>W.java</sourceFile><mutatedClass>a.b.W</mutatedClass><mutatedMethod>f</mutatedMethod><lineNumber>7</lineNumber><mutator>org.pitest.mutationtest.engine.gregor.mutators.MathMutator</mutator><index>3</index><killingTests>a.T.one()|a.T.two()</killingTests><description>Replaced integer addition with subtraction</description></mutation>\n<mutation detected=\"false\" status=\"NO_COVERAGE\"><sourceFile>Top.kt</sourceFile><mutatedClass>TopKt</mutatedClass><mutatedMethod>g</mutatedMethod><lineNumber>x</lineNumber><mutator>VoidMethodCallMutator</mutator><description>removed call to a/B::run &amp;&lt;&gt;&quot;&apos;</description></mutation>\n<mutation status='SURVIVED'><mutator>org.pitest.mutationtest.engine.gregor.mutators.returns.NullReturnValsMutator</mutator><description>replaced return value with null for a/W::name</description></mutation>\n</mutations>\n";
        let pit = parse_pit_report(report);
        assert_eq!(pit.len(), 3);
        assert_eq!(pit[0].file, "a/b/W.java");
        assert_eq!((pit[0].line, pit[0].index), (7, 3));
        assert_eq!(pit[0].mutator, "Math");
        assert_eq!(pit[0].key, "+>-");
        assert_eq!(pit[0].killing_tests, ["a.T.one()", "a.T.two()"]);
        assert!(pit[0].detected());
        assert_eq!(pit[1].file, "Top.kt");
        assert_eq!(pit[1].line, 0);
        assert_eq!(pit[1].status, "NO_COVERAGE");
        assert!(!pit[1].detected());
        assert_eq!(pit[1].description, "removed call to a/B::run &<>\"'");
        assert_eq!(pit[2].mutator, "NullReturnVals");
        assert_eq!(pit[2].file, "");
        assert!(pit[2].killing_tests.is_empty());
    }

    #[test]
    fn pit_only_kotlin_mutants_are_put_down_to_what_the_compiler_wrote() {
        let mutant = |line: u32, mutator: &str| PitMutant {
            file: "a/B.kt".into(),
            line,
            mutator: mutator.into(),
            key: String::new(),
            class: String::new(),
            method: "f".into(),
            description: String::new(),
            status: "KILLED".into(),
            killing_tests: Vec::new(),
            index: 0,
        };
        let source = "fun f(c: Char, a: List<Int>, x: Any) {\n    require(a.size > 1) { \"m\" }\n    while (c in '0'..'9') {}\n    if (a.isNotEmpty()) {}\n    when (x) {\n        is String -> {\n        }\n    }\n    g()\n}\n";
        let category = |line| pit_only_category(&mutant(line, "NegateConditionals"), Some(source));
        assert_eq!(category(2), "a Kotlin require or check's own jump");
        assert_eq!(category(3), "a Kotlin range check");
        assert_eq!(
            category(4),
            "an inlined standard library function's own condition"
        );
        assert_eq!(category(6), "a `when` branch's comparison with its subject");
        assert_eq!(category(9), "other");
        let filled = "val widths = IntArray(n) { 1 }\n";
        assert_eq!(
            pit_only_category(&mutant(1, "ConditionalsBoundary"), Some(filled)),
            "a Kotlin array constructor's loop"
        );
        assert_eq!(
            pit_only_category(&mutant(2, "VoidMethodCall"), Some(source)),
            "other"
        );
        let compiled = "class B(val name: String) {\n    fun f(a: String?) = a?.length\n    fun g() {\n        for (i in 0 until 3) {}\n    }\n}\n";
        let mut getter = mutant(1, "EmptyObjectReturnVals");
        getter.method = "getName".into();
        assert_eq!(
            pit_only_category(&getter, Some(compiled)),
            "a Kotlin property's generated getter"
        );
        let mut checked = mutant(2, "VoidMethodCall");
        checked.description = "removed call to kotlin/jvm/internal/Intrinsics::checkNotNull".into();
        assert_eq!(
            pit_only_category(&checked, Some(compiled)),
            "a Kotlin null check the compiler wrote"
        );
        assert_eq!(
            pit_only_category(&mutant(2, "NegateConditionals"), Some(compiled)),
            "a Kotlin null-safe operator"
        );
        assert_eq!(
            pit_only_category(&mutant(4, "Increments"), Some(compiled)),
            "a Kotlin for loop's counter"
        );
        assert_eq!(
            pit_only_category(&mutant(40, "Math"), Some(compiled)),
            "inlined code (a line past the file's end)"
        );
        assert_eq!(
            pit_only_category(&mutant(1, "Math"), None),
            "inlined code (a line past the file's end)"
        );

        // Code kotlinc writes for an assignment, an inlined lambda, a
        // spread, a lambda's class, a `when`, a `requireNotNull`.
        let more = "class B {
    fun f(b: B, c: IntArray, a: List<String>) {
        b.isOpen = true
        name = \"x\"
        c[0]--
        tags += a
        a.forEach { g(it) }
        a.forEach(::g)
        a.map(String::length).filter({ it > 1 })
        h(*a.toTypedArray())
        when (a.size) {
        val x = requireNotNull(a.firstOrNull())
        if (name.isEmpty()) {}
        b.isOpen = g(\"y\")
    }
}
";
        let called = |line: u32, method: &str| {
            let mut call = mutant(line, "VoidMethodCall");
            call.description = format!("removed call to a/B::{method}");
            pit_only_category(&call, Some(more))
        };
        let assignment = "a Kotlin property assignment's setter or operator call";
        assert_eq!(called(3, "setOpen$module"), assignment);
        assert_eq!(called(4, "access$setName$p"), assignment);
        assert_eq!(called(5, "set"), assignment);
        assert_eq!(called(6, "putAll"), assignment);
        assert_eq!(called(14, "setOpen"), assignment);
        // A setter of another name, and a call that is no setter.
        assert_eq!(called(3, "setName"), "other");
        assert_eq!(called(3, "set"), "other");
        assert_eq!(called(13, "setOpen"), "other");
        assert_eq!(called(6, "remove"), "other");
        let inlined = "code a standard library function inlines with its lambda";
        assert_eq!(called(7, "g"), inlined);
        assert_eq!(called(8, "g"), inlined);
        assert_eq!(called(9, "g"), inlined);
        let mut spread = mutant(10, "VoidMethodCall");
        spread.description = "removed call to kotlin/jvm/internal/SpreadBuilder::add".into();
        assert_eq!(
            pit_only_category(&spread, Some(more)),
            "a Kotlin spread operator's copying"
        );
        let returned = |method: &str, class: &str| {
            let mut returned = mutant(13, "NullReturnVals");
            returned.method = method.into();
            returned.class = class.into();
            pit_only_category(&returned, Some(more))
        };
        let lambda = "a Kotlin lambda's or function reference's own return";
        assert_eq!(returned("invoke", "a.B$f$1"), lambda);
        assert_eq!(returned("get", "a.B$f$ref$1"), lambda);
        assert_eq!(returned("invoke", "a.B"), "other");
        assert_eq!(returned("f", "a.B$f$1"), "other");
        let mut boolean = mutant(13, "BooleanTrueReturnVals");
        boolean.method = "invoke".into();
        boolean.class = "a.B$1".into();
        assert_eq!(pit_only_category(&boolean, Some(more)), lambda);
        let mut primitive = mutant(13, "PrimitiveReturns");
        primitive.method = "invoke".into();
        primitive.class = "a.B$1".into();
        assert_eq!(pit_only_category(&primitive, Some(more)), lambda);
        let mut flag = mutant(1, "BooleanFalseReturnVals");
        flag.method = "isOpen".into();
        assert_eq!(
            pit_only_category(&flag, Some("val isOpen: Boolean = true\n")),
            "a Kotlin property's generated getter"
        );
        let conditional =
            |line: u32, mutator: &str| pit_only_category(&mutant(line, mutator), Some(more));
        assert_eq!(
            conditional(11, "ConditionalsBoundary"),
            "a `when` branch's comparison with its subject"
        );
        assert_eq!(
            conditional(12, "NegateConditionals"),
            "a Kotlin require or check's own jump"
        );
        assert_eq!(
            conditional(13, "ConditionalsBoundary"),
            "an inlined standard library function's own condition"
        );
        assert_eq!(conditional(13, "Math"), "other");
        let java = PitMutant {
            file: "a/B.java".into(),
            ..mutant(3, "VoidMethodCall")
        };
        assert_eq!(pit_only_category(&java, Some(more)), "other");
    }

    /// A name inlining is not: a word that holds one, a call without a
    /// lambda.
    #[test]
    fn only_a_call_given_a_lambda_or_reference_inlines_one() {
        assert!(inlines_a_lambda("xs.forEach { }"));
        assert!(inlines_a_lambda("xs.use(::close)"));
        assert!(inlines_a_lambda("repeat(3) { }"));
        assert!(!inlines_a_lambda("xs.forEach(g)"));
        assert!(!inlines_a_lambda("mapper { }"));
        assert!(!inlines_a_lambda("remap { }"));
        assert!(!inlines_a_lambda("val count = 3"));
    }

    /// kotlinc names a call or a method after its module where it is
    /// `internal`, after the function around it where it is local, and so
    /// on: PIT's mutant is joined by every name its method's holds.
    #[test]
    fn a_mangled_method_is_joined_by_each_name_it_holds() {
        let mutant = |mutator: &str, key: &str| PitMutant {
            file: "a/B.kt".into(),
            line: 1,
            mutator: mutator.into(),
            key: key.into(),
            class: String::new(),
            method: String::new(),
            description: String::new(),
            status: "KILLED".into(),
            killing_tests: Vec::new(),
            index: 0,
        };
        assert_eq!(
            pit_keys(&mutant("VoidMethodCall", "access$emit$module$default")),
            ["access$emit$module$default", "emit", "module"]
        );
        assert_eq!(
            pit_keys(&mutant("BooleanTrueReturnVals", "isOpen$module")),
            ["isOpen$module", "isOpen", "module"]
        );
        assert_eq!(
            pit_keys(&mutant("PrimitiveReturns", "size$module")),
            ["size$module", "size", "module"]
        );
        assert_eq!(pit_keys(&mutant("VoidMethodCall", "emit")), ["emit"]);
        assert_eq!(pit_keys(&mutant("Math", "+$>-")), ["+$>-"]);
    }

    /// kotlinc increments a local with `IINC` (PIT's `Increments`) unless a
    /// lambda reads it, inlined or not: then it loads, adds and stores
    /// (`Math`), as kotlinpoet's `CodeBlock` showed.
    #[test]
    fn a_kotlin_local_a_lambda_reads_is_incremented_by_math() {
        let file = "fixture/Step.kt";
        let source = "fun f(s: String): Int {\n    var p = 0\n    var q = 0\n    while (p < s.length) {\n        require(p >= 0) { \"at $q\" }\n        p++\n        q++\n        g()\n    }\n    return p + q\n}\n\nfun g() {}\n";
        let index = ProjectIndex::of([(file, source)]);
        let ids = file_mutants(file, source, &index)
            .unwrap()
            .mutants
            .iter()
            .map(|mutant| mutant_id(file, mutant))
            .collect::<Vec<_>>();
        assert!(
            ids.contains(&format!("{file}:6:9-6:12:Increments:--")),
            "{ids:#?}"
        );
        assert!(
            ids.contains(&format!("{file}:7:9-7:12:Math:--")),
            "{ids:#?}"
        );
    }

    /// Kotlin constructs PIT mutates as kotlinc compiles them: an explicit
    /// getter returns its property's type (`getArea`, `getEmpty`), an enum
    /// entry's body is a class of its own, `x in names` branched on is a
    /// call's jump, and a private function only a companion's or object's
    /// initialization calls is not mutated.
    #[test]
    fn kotlin_accessors_enum_bodies_and_initializers() {
        let file = "tour/Tour.kt";
        let source = "package tour\n\nenum class Op(val sign: String) {\n    PLUS(\"+\") {\n        override fun apply(a: Int, b: Int) = a + b\n    },\n    MINUS(\"-\") {\n        override fun apply(a: Int, b: Int) = a - b\n    };\n\n    abstract fun apply(a: Int, b: Int): Int\n}\n\nclass Box(private var size: Int) {\n    var area: Int\n        get() = size * size\n        set(value) {\n            size = value / 2\n        }\n\n    val empty: Boolean\n        get() {\n            return size == 0\n        }\n\n    companion object {\n        private val DEFAULT = build(3)\n\n        private fun build(n: Int): Int = n * 2\n\n        fun make(): Box = Box(DEFAULT + 1)\n    }\n}\n\nobject Registry {\n    private val names = load()\n\n    private fun load(): List<String> = listOf(\"a\") + \"b\"\n\n    fun has(name: String): Boolean = name in names && !name.isEmpty()\n}\n";
        let index = ProjectIndex::of([(file, source)]);
        let ids = file_mutants(file, source, &index)
            .unwrap()
            .mutants
            .iter()
            .map(|mutant| mutant_id(file, mutant))
            .collect::<Vec<_>>();
        for expected in [
            ":5:46-5:51:PrimitiveReturns:0",
            ":8:46-8:51:Math:+",
            ":16:17-16:28:PrimitiveReturns:0",
            ":18:20-18:29:Math:*",
            ":23:20-23:29:BooleanTrueReturnVals:true",
            ":40:38-40:51:NegateConditionals:negated",
        ] {
            assert!(
                ids.contains(&format!("{file}{expected}")),
                "{expected}: {ids:#?}"
            );
        }
        // `build(3)` and `load()` run only as the statics are set up.
        assert!(
            !ids.iter()
                .any(|id| id.contains("Tour.kt:29:") || id.contains("Tour.kt:38:")),
            "{ids:#?}"
        );
        let mutant = |method: &str| {
            let listed = file_mutants(file, source, &index).unwrap().mutants;
            listed
                .into_iter()
                .filter(|mutant| mutant.method == method)
                .count()
        };
        assert_eq!(mutant("getArea"), 2);
        assert_eq!(mutant("setArea"), 1);
        assert_eq!(mutant("getEmpty"), 2);
    }

    #[test]
    fn java_mutants_pit_made_on_public_projects() {
        let file = "fixture/Reach.java";
        let source = "package fixture;\n\nimport java.lang.reflect.Field;\nimport java.security.AccessController;\nimport java.security.PrivilegedExceptionAction;\nimport java.util.Arrays;\nimport java.util.function.Supplier;\n\nclass Reach {\n  static final Object VALUE;\n\n  static {\n    try {\n      VALUE = AccessController.doPrivileged(new PrivilegedExceptionAction<Object>() {\n        @Override\n        public Object run() throws Exception {\n          final Field f = Reach.class.getDeclaredField(\"VALUE\");\n          f.setAccessible(true);\n          return f;\n        }\n      });\n    } catch (Exception e) {\n      throw new IllegalStateException(e);\n    }\n  }\n\n  static String[] names(final Object[] values) {\n    final String[] out = new String[values.length];\n    Arrays.setAll(out, i -> String.valueOf(values[i]));\n    return out;\n  }\n\n  <T> T value(final T defaultValue) {\n    return get(() -> defaultValue);\n  }\n\n  <T> T get(final Supplier<T> supplier) {\n    return supplier.get();\n  }\n}\n";
        let index = ProjectIndex::of([(file, source)]);
        let ids = file_mutants(file, source, &index)
            .unwrap()
            .mutants
            .iter()
            .map(|mutant| mutant_id(file, mutant))
            .collect::<Vec<_>>();
        for (line, mutator) in [
            (18, "VoidMethodCall"),
            (19, "NullReturnVals"),
            (29, "VoidMethodCall"),
            (34, "NullReturnVals"),
        ] {
            assert!(
                ids.iter()
                    .any(|id| id.starts_with(&format!("{file}:{line}:")) && id.contains(mutator)),
                "{line} {mutator}: {ids:#?}"
            );
        }
        // The static initializer's own code is not mutated.
        assert!(
            !ids.iter().any(|id| (12..=25)
                .contains(&id.split(':').nth(1).unwrap().parse::<u32>().unwrap())
                && !(18..=19).contains(&id.split(':').nth(1).unwrap().parse::<u32>().unwrap())),
            "{ids:#?}"
        );
    }

    #[test]
    fn kotlin_mutants_are_pit_s_but_the_compiler_s_own_code() {
        let (only_pit, only_supercov) = parity("kotlin", &["fixture/Ops.kt"]);
        for missing in &only_pit {
            println!("{missing}");
        }
        assert_eq!(only_supercov, Vec::<String>::new());
        // What kotlinc writes that the source does not say: a property's
        // getter, a range loop's counter, a `when` subject's comparisons,
        // `?.`, `?:` and `!!`'s null checks, and a standard library function
        // inlined at a line past the file's end.
        assert_eq!(only_pit.len(), 23, "{only_pit:#?}");
    }

    // Coverage of PIT's report, the join with it and its own categories.

    const TWICE: &str = "class A {\n  int f(int n) {\n    return n * 2;\n  }\n}\n";

    fn twice_mutants() -> Vec<JvmMutant> {
        let index = ProjectIndex::of([("src/A.java", TWICE)]);
        file_mutants("src/A.java", TWICE, &index).unwrap().mutants
    }

    fn pit_mutant(file: &str, line: u32, mutator: &str) -> PitMutant {
        PitMutant {
            file: file.into(),
            line,
            mutator: mutator.into(),
            key: String::new(),
            class: String::new(),
            method: "f".into(),
            description: String::new(),
            status: "KILLED".into(),
            killing_tests: Vec::new(),
            index: 0,
        }
    }

    /// A file with no mutants places no probe; a probe with no edits takes
    /// its slot and places nothing, and links no listed mutant where there
    /// is none.
    #[test]
    fn a_probe_with_nothing_to_place_takes_its_slot_alone() {
        let mut next_probe = 4;
        let (edits, sites) = probe_edits("src/A.java", TWICE, &[], &mut [], &mut next_probe);
        assert!(edits.is_empty() && sites.is_empty());
        assert_eq!(next_probe, 4);
        let mut probed = twice_mutants()
            .into_iter()
            .find(|mutant| mutant.probe.is_some())
            .expect("a probed mutant");
        let kind = probed.probe.as_ref().unwrap().kind;
        probed.probe.as_mut().unwrap().edits.clear();
        let (edits, sites) = probe_edits("src/A.java", TWICE, &[probed], &mut [], &mut next_probe);
        assert!(edits.is_empty());
        assert_eq!(next_probe, 5);
        assert_eq!(
            sites
                .into_iter()
                .map(|(slot, site)| (slot, site.kind))
                .collect::<Vec<_>>(),
            [(5, kind)]
        );
    }

    /// A mutant deferred in a lambda its statement holds has no statement
    /// of its own; a lambda the statement does not wholly hold leaves it
    /// the statement's.
    #[test]
    fn a_deferred_mutant_s_point_is_its_own_unless_it_holds_the_lambda() {
        let mut mutant = twice_mutants().remove(0);
        mutant.range = (12, 14);
        let spans = [("p".to_owned(), PointKind::Statement, 10, 20)];
        for (lambda, statement) in [
            ((11, 15), None),
            ((5, 15), Some("p")),
            ((12, 30), Some("p")),
        ] {
            mutant.deferred = Some(lambda);
            assert_eq!(
                candidate(&mutant, &spans).statement.as_deref(),
                statement,
                "{lambda:?}"
            );
        }
    }

    /// Only a Java or Kotlin file that parses has mutants; a run's file
    /// that no longer parses is named as changed, and a run with no Java or
    /// Kotlin file lists nothing.
    #[test]
    fn a_file_that_does_not_parse_has_no_mutants() {
        let index = ProjectIndex::of([("src/A.java", TWICE)]);
        assert_eq!(
            file_mutants("notes.txt", "", &index).err().as_deref(),
            Some("notes.txt is not Java or Kotlin")
        );
        let broken = file_mutants("src/B.java", "class B {", &index)
            .err()
            .unwrap();
        assert!(broken.starts_with("JVM parse error"), "{broken}");
        let root = std::env::temp_dir().join(format!("supercov-jvm-broken-{}", std::process::id()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/B.java"), "class B {").unwrap();
        let none = list_mutants(
            &root,
            &BTreeMap::from([("notes.txt".to_owned(), BTreeSet::new())]),
        );
        let changed = list_mutants(
            &root,
            &BTreeMap::from([("src/B.java".to_owned(), BTreeSet::new())]),
        );
        std::fs::remove_dir_all(&root).unwrap();
        assert!(none.0.is_empty() && none.1.is_empty());
        assert!(changed.0.is_empty());
        assert_eq!(changed.1, ["src/B.java"]);
    }

    /// An empty report has no mutants; a tag left open reads as empty.
    #[test]
    fn an_empty_report_or_an_open_tag_reads_as_nothing() {
        assert!(parse_pit_report("").is_empty());
        let open = parse_pit_report("<mutation status='KILLED'><mutator>Math");
        assert_eq!(open.len(), 1);
        assert_eq!((open[0].mutator.as_str(), open[0].key.as_str()), ("", ""));
        assert_eq!(open[0].status, "KILLED");
    }

    /// The key of each of PIT's descriptions: every operation its math
    /// mutator names, an increment's step and a return's method; a
    /// description without a replacement has none.
    #[test]
    fn each_of_pit_s_descriptions_gives_its_key() {
        for (description, key) in [
            ("Replaced integer division with multiplication", "/>*"),
            ("Replaced integer modulus with multiplication", "%>*"),
            ("Replaced bitwise AND with OR", "&>|"),
            ("Replaced bitwise OR with AND", "|>&"),
            ("Replaced XOR with AND", "^>&"),
            ("Replaced Shift Left with Shift Right", "<<>>>"),
            ("Replaced Shift Right with Shift Left", ">>><<"),
            ("Replaced Unsigned Shift Right with Shift Left", ">>>><<"),
            ("Replaced long negation with nothing", "negation>nothing"),
            ("Replaced integer addition", ""),
        ] {
            assert_eq!(pit_key("Math", description), key, "{description}");
        }
        assert_eq!(pit_key("Increments", "Changed increment from 1 to -1"), "1");
        assert_eq!(
            pit_key(
                "PrimitiveReturns",
                "replaced int return with 0 for a/B::size"
            ),
            "size"
        );
    }

    /// A return PIT reports on a line outside its statement is paired by
    /// its method in the last pass; an operation there is not, and both
    /// sides keep theirs. A file with no mutants joins nothing, and a
    /// mutant of a file the run does not have is PIT's alone.
    #[test]
    fn a_return_far_from_its_statement_is_paired_by_its_method() {
        let mutants = twice_mutants();
        let mut pit = mutants
            .iter()
            .map(|mutant| PitMutant {
                key: mutant.key.clone(),
                ..pit_mutant("A.java", 40, mutant.mutator.name())
            })
            .collect::<Vec<_>>();
        pit.push(pit_mutant("Elsewhere.java", 3, "Math"));
        let ours = BTreeMap::from([
            ("src/A.java".to_owned(), mutants.clone()),
            ("src/B.java".to_owned(), Vec::new()),
        ]);
        let join = join_pit(&ours, &pit);
        let returns = |want: bool| {
            mutants
                .iter()
                .enumerate()
                .filter(|(_, mutant)| is_return(mutant.mutator) == want)
                .map(|(at, mutant)| (mutant_id("src/A.java", mutant), at))
                .collect::<Vec<_>>()
        };
        assert!(!returns(true).is_empty() && !returns(false).is_empty());
        assert_eq!(join.pairs, returns(true));
        assert_eq!(
            join.only_supercov,
            returns(false)
                .into_iter()
                .map(|(id, _)| id)
                .collect::<Vec<_>>()
        );
        let mut only_pit = returns(false)
            .into_iter()
            .map(|(_, at)| at)
            .collect::<Vec<_>>();
        only_pit.push(mutants.len());
        assert_eq!(join.only_pit, only_pit);
    }

    /// What decides each of PIT's own Kotlin categories, beside what does
    /// not: a getter is a returning one of Kotlin's `val` or `var`; `?:` and
    /// `!!` are null-safe operators like `?.`; an array constructor loops
    /// only with its lambda; a `when` branch is an arrow without a brace or
    /// an `is` test; an indexed `set` is an assignment, an increment or a
    /// decrement; `putAll` and `add` are `+=` and `-=`; and `in` checks a
    /// range only where one is written.
    #[test]
    fn pit_s_own_kotlin_categories_turn_on_what_the_line_writes() {
        let category = |file: &str, method: &str, mutator: &str, called: &str, line: &str| {
            let mutant = PitMutant {
                method: method.into(),
                description: format!("removed call to a/B::{called}"),
                ..pit_mutant(file, 1, mutator)
            };
            pit_only_category(&mutant, Some(line))
        };
        let kotlin = |mutator: &str, line: &str| category("a/B.kt", "f", mutator, "", line);
        let getter = "a Kotlin property's generated getter";
        assert_eq!(
            category(
                "a/B.kt",
                "getSize",
                "PrimitiveReturns",
                "",
                "    var size = 1"
            ),
            getter
        );
        assert_eq!(
            category(
                "a/B.kt",
                "getSize",
                "VoidMethodCall",
                "",
                "    val size = 1"
            ),
            "other"
        );
        assert_eq!(
            category(
                "a/B.java",
                "getSize",
                "PrimitiveReturns",
                "",
                "    var size = 1"
            ),
            "other"
        );
        let null_safe = "a Kotlin null-safe operator";
        assert_eq!(
            kotlin("NegateConditionals", "    val n = x ?: 0"),
            null_safe
        );
        assert_eq!(
            kotlin("NegateConditionals", "    val n = x!!.length"),
            null_safe
        );
        assert_eq!(kotlin("Math", "    val a = IntArray(n)"), "other");
        let when = "a `when` branch's comparison with its subject";
        assert_eq!(kotlin("Math", "        1 -> 2"), when);
        assert_eq!(kotlin("Math", "        1 -> {"), "other");
        assert_eq!(kotlin("Math", "        is Int"), when);
        assert_eq!(kotlin("Math", "        !is Int"), when);
        assert_eq!(kotlin("NegateConditionals", "        x > 1 -> {"), when);
        let assigned = "a Kotlin property assignment's setter or operator call";
        let call =
            |called: &str, line: &str| category("a/B.kt", "f", "VoidMethodCall", called, line);
        assert_eq!(call("set", "    counts[i] = n"), assigned);
        assert_eq!(call("set", "    counts[i]++"), assigned);
        assert_eq!(call("add", "    xs += y"), assigned);
        assert_eq!(call("putAll", "    m -= k"), assigned);
        assert_eq!(call("putAll", "    m.putAll(o)"), "other");
        assert_eq!(
            kotlin("NegateConditionals", "    if (x in names) {}"),
            "other"
        );
        assert!(!inlines_a_lambda("my_forEach { }"));
    }
}
