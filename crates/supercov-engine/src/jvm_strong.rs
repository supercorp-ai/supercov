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
            let linked = match probe.kind {
                ProbeKind::Compare => {
                    matches!(
                        other.mutator,
                        crate::jvm_mutants::Mutator::ConditionalsBoundary
                            | crate::jvm_mutants::Mutator::NegateConditionals
                    ) && {
                        link.compare = Some(site.clone());
                        true
                    }
                }
                ProbeKind::Operation => {
                    std::ptr::eq(other, mutant) && {
                        link.probe = Some(site.clone());
                        true
                    }
                }
                ProbeKind::Return => {
                    is_return(other.mutator) && {
                        link.probe = Some(site.clone());
                        true
                    }
                }
            };
            let _ = linked;
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
    let tree = crate::jvm_instrumenter::parse(source, language).map_err(|e| e.to_string())?;
    let mut next_probe = 0;
    let mut next_decision = 0;
    let obligations = crate::jvm_instrumenter::build_jvm_obligations(
        file,
        source,
        language,
        &mut next_probe,
        &mut next_decision,
    )
    .map_err(|e| e.to_string())?;
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

/// The innermost point of `kind` whose span holds `range`.
fn innermost(
    spans: &[(String, PointKind, usize, usize)],
    kind: PointKind,
    range: (usize, usize),
) -> Option<String> {
    spans
        .iter()
        .filter(|(_, point, start, end)| *point == kind && *start <= range.0 && range.1 <= *end)
        .min_by_key(|(_, _, start, end)| end - start)
        .map(|(id, ..)| id.clone())
}

pub(crate) fn candidate(
    mutant: &JvmMutant,
    spans: &[(String, PointKind, usize, usize)],
) -> CandidateMutant {
    // A point that holds the lambda the mutant is deferred in ran when the
    // lambda was made, not when it ran: such a mutant's reach is unknown,
    // and every test is its candidate, unless its probe says otherwise.
    let own = |kind: PointKind| {
        let point = innermost(spans, kind, mutant.range)?;
        let span = spans.iter().find(|(id, ..)| *id == point)?;
        match mutant.deferred {
            Some(lambda) if span.2 <= lambda.0 && lambda.1 <= span.3 => None,
            _ => Some(point),
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
        by_group
            .entry((file, mutant.mutator.clone(), mutant.key.clone()))
            .or_default()
            .push(at);
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
    // Three passes, each taking what the one before left: the exact line,
    // then any line of the mutant's statement, then (for a return) any line
    // of its method.
    for pass in 0..4 {
        let mut left = Vec::new();
        for (file, mutant, key) in unmatched {
            let group =
                by_group.get(&(file.clone(), mutant.mutator.name().to_owned(), key.clone()));
            let found = group.and_then(|group| {
                group.iter().copied().find(|at| {
                    !taken[*at]
                        && match pass {
                            0 => pit[*at].line == mutant.start.0,
                            // javac gives an operator on a continuation line
                            // the line of the operand before it, or of the
                            // statement.
                            1 => (mutant.lines.0.saturating_sub(3)
                                ..=mutant.lines.1.max(mutant.end.0))
                                .contains(&pit[*at].line),
                            // kotlinc puts a condition's jumps on its last
                            // line: the statement's every line.
                            2 => (mutant.lines.0..=mutant.statement_end).contains(&pit[*at].line),
                            _ => is_return(mutant.mutator),
                        }
                })
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

/// What PIT made that no source can say, by the compiler construct it came
/// from: a disagreement explained rather than a missed mutant.
pub fn pit_only_category(mutant: &PitMutant, source: Option<&str>) -> &'static str {
    let line = source.and_then(|source| source.lines().nth(mutant.line.saturating_sub(1) as usize));
    let Some(line) = line else {
        return "inlined code (a line past the file's end)";
    };
    let kotlin = mutant.file.ends_with(".kt");
    if mutant.method.starts_with("get")
        && (mutant.mutator.ends_with("ReturnVals") || mutant.mutator == "PrimitiveReturns")
        && kotlin
        && (line.contains("val ") || line.contains("var "))
    {
        return "a Kotlin property's generated getter";
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
    if kotlin && line.contains("->") && !line.contains('{') {
        return "a `when` branch's comparison with its subject";
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

    /// `PROJECT=<dir> PIT=<mutations.xml>`: a project's mutants joined with
    /// PIT's report, printed: what each side lists that the other does not.
    #[test]
    #[ignore]
    fn join_project() {
        let root = std::path::PathBuf::from(std::env::var("PROJECT").unwrap());
        let pit =
            parse_pit_report(&std::fs::read_to_string(std::env::var("PIT").unwrap()).unwrap());
        let files = crate::jvm_project::discover_jvm_files(&root)
            .unwrap()
            .sources
            .into_iter()
            .map(|(file, _)| file)
            .collect::<Vec<_>>();
        let index = index_of(&root, &files);
        let ours = files
            .iter()
            .filter_map(|file| {
                let source = std::fs::read_to_string(root.join(file)).ok()?;
                Some((
                    file.clone(),
                    file_mutants(file, &source, &index).ok()?.mutants,
                ))
            })
            .collect::<BTreeMap<_, _>>();
        let join = join_pit(&ours, &pit);
        println!(
            "pit {} ours {} matched {}",
            pit.len(),
            ours.values().map(Vec::len).sum::<usize>(),
            join.pairs.len()
        );
        for at in &join.only_pit {
            let m = &pit[*at];
            println!("PIT  {}:{} {} {}", m.file, m.line, m.mutator, m.description);
        }
        for id in &join.only_supercov {
            println!("OURS {id}");
        }
    }

    /// `PROBE=<file>`: the file as a measured run with its mutants listed
    /// writes it, coverage probes and value probes both.
    #[test]
    #[ignore]
    fn print_probed() {
        let path = std::env::var("PROBE").unwrap();
        let source = std::fs::read_to_string(&path).unwrap();
        let language = language_of(&path).unwrap();
        let index = ProjectIndex::of([(path.as_str(), source.as_str())]);
        let mut next_probe = 0;
        let mut next_decision = 0;
        let obligations = crate::jvm_instrumenter::build_jvm_obligations(
            &path,
            &source,
            language,
            &mut next_probe,
            &mut next_decision,
        )
        .unwrap();
        let tree = crate::jvm_instrumenter::parse(&source, language).unwrap();
        let found = crate::jvm_mutants::enumerate(&source, &tree, language, &index);
        let mut listed = found
            .iter()
            .map(|mutant| candidate(mutant, &obligations.point_spans))
            .collect::<Vec<_>>();
        let (edits, _) = probe_edits(&path, &source, &found, &mut listed, &mut next_probe);
        let mut all = obligations.edits;
        if std::env::var("COVERAGE_ONLY").is_err() {
            all.extend(edits);
        }
        print!("{}", crate::jvm_instrumenter::rewrite_plain(&source, &all));
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
}
