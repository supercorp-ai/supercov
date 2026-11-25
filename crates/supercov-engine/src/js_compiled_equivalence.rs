//! Trivial Compiler Equivalence (Papadakis et al., ICSE 2015) for the
//! JavaScript and TypeScript mutants a strong pass would run.
//!
//! A mutant's file is compiled with the mutant applied in place -- its
//! TypeScript erased, its JSX made calls, then optimised by oxc's minifier
//! with options that assume nothing of the program the language does not
//! say -- and the code compared with the original file's, compiled the same
//! way. The compiler is deterministic and preserves meaning, so where the
//! two codes are the same, the two programs are the same program: no test
//! can tell the mutant from the original, and it is `equivalent` without
//! running. Where two mutants of one file compile to the same code they are
//! one program, and one run decides both (`duplicates`).
//!
//! Anything that keeps a file from compiling, the mutant's node not being
//! found, or an early error in either program leaves the mutant to run: a
//! mutant is only ever decided by an exact match.
//! `docs/design/mutants.md` ("Compiled the same") says why it is sound and
//! what it assumes.

use std::collections::BTreeMap;
use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_minifier::{
    CompressOptions, CompressOptionsKeepNames, CompressOptionsUnused, Minifier, MinifierOptions,
    TreeShakeOptions,
};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_transformer::{TransformOptions, Transformer, TypeScriptOptions};

use crate::js_mutants::LedgerMutant;
use crate::strong_plan::{Decided, StrongPlan, Verdict};

/// The rule (a plan's `reason`) of a mutant whose file compiles to the same
/// code as the original.
pub const COMPILES_TO_ORIGINAL: &str = "compiles-to-original";

/// The start of a mutant's reason where it took the verdict of another
/// mutant whose file compiles to the same code: `same-code-as:<id>`.
pub const SAME_CODE_AS: &str = "same-code-as:";

/// The verdicts a mutant compiled to the same code as another takes from it:
/// what one run of the program decides for every copy of it. An `error`, or
/// no verdict yet, decides nothing.
const SHARED: [&str; 3] = ["killed", "survived", "timeout"];

/// What comparing compiled code decided of a plan.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SameCode {
    /// How many mutants compile to the original: decided `equivalent`.
    pub equivalent: usize,
    /// How many of them the pass would have run: the others' verdicts the
    /// plan had already decided (survived, as no test infected them, or no
    /// coverage), and they are now `equivalent` instead.
    pub equivalent_to_run: usize,
    /// The mutants taken out of the plan because another compiles to the
    /// same code, by id, each with the id of the one that runs for it.
    pub duplicates: BTreeMap<String, String>,
    /// The test time, in the measured run, of the candidates of the mutants
    /// not run: each equivalent one would have run every candidate and
    /// survived, and a duplicate at most every candidate.
    pub saved_ms: u64,
    /// The files whose original did not compile (or parse): their mutants
    /// run as before.
    pub not_compiled: Vec<String>,
}

/// `decide_by_compiled_code` for a stored run: a file is compared only where
/// its source under `root` has the bytes the run measured, by its source
/// manifest. A run without one decides nothing.
pub fn decide_for_run(
    plan: &mut StrongPlan,
    root: &Path,
    run: &crate::run_store::StoredRun,
    settled: &BTreeMap<String, Verdict>,
) -> SameCode {
    let files = crate::source_manifest::load_manifest(run)
        .map(|stored| stored.manifest.files)
        .unwrap_or_default();
    decide_by_compiled_code(plan, settled, |file| {
        let source = std::fs::read_to_string(root.join(file)).ok()?;
        let measured = files.get(file)?;
        crate::source_manifest::FileFingerprint::of(&source)
            .same_bytes(measured)
            .then_some(source)
    })
}

/// Compares the compiled code of `plan`'s JavaScript mutants with the
/// original's, and of its undecided ones with each other's. A mutant that
/// compiles to the original is decided `equivalent`, one the plan decided
/// `survived` or `no-coverage` as well: it is a fact about every test, where
/// those are about this run's. Of undecided mutants of one file that compile
/// to the same code, the first in the plan stays and the rest are taken out
/// (`SameCode::duplicates`), to take its verdict (`shared_verdicts`). A
/// mutant `settled` holds a verdict for is left as it is, and where that
/// verdict is one a duplicate takes (killed, survived, timeout), it is the
/// one that stays: its duplicates take its verdict at once.
///
/// `source_of` gives a file's source exactly as the run measured it, or
/// `None` where it cannot (the file changed since): such a file's mutants
/// are left as they are.
pub fn decide_by_compiled_code(
    plan: &mut StrongPlan,
    settled: &BTreeMap<String, Verdict>,
    source_of: impl Fn(&str) -> Option<String> + Sync,
) -> SameCode {
    let shared = |id: &String| {
        settled
            .get(id)
            .is_some_and(|verdict| SHARED.contains(&verdict.status.as_str()))
    };
    let mut by_file = BTreeMap::<&str, Vec<usize>>::new();
    for (position, mutant) in plan.mutants.iter().enumerate() {
        let compared = !settled.contains_key(&mutant.id) || shared(&mutant.id);
        if compared
            && mutant.decided != Some(Decided::Equivalent)
            && mutant.language == crate::mutant_list::JAVASCRIPT
        {
            by_file
                .entry(mutant.file.as_str())
                .or_default()
                .push(position);
        }
    }
    let mut same = SameCode::default();
    let files = by_file
        .into_iter()
        .filter_map(|(file, positions)| {
            let source = source_of(file)?;
            let Some(listed) = listed_by_id(file, &source) else {
                same.not_compiled.push(file.to_owned());
                return None;
            };
            // Those with a verdict first: each is the one its duplicates
            // take theirs from.
            let mut mutants = positions
                .into_iter()
                .filter_map(|position| {
                    Some((position, listed.get(&plan.mutants[position].id)?.clone()))
                })
                .collect::<Vec<_>>();
            mutants.sort_by_key(|(position, _)| !shared(&plan.mutants[*position].id));
            Some((file.to_owned(), source, mutants))
        })
        .collect::<Vec<_>>();
    // One job per compile -- each file's original, then each mutant -- so
    // the work spreads over every core however it falls across files.
    let jobs = files
        .iter()
        .enumerate()
        .flat_map(|(at, (_, _, mutants))| {
            std::iter::once((at, None)).chain(mutants.iter().map(move |(_, m)| (at, Some(m))))
        })
        .collect::<Vec<_>>();
    let compiled = crate::threads::parallel_map(&jobs, |(at, mutant)| {
        let (file, source, _) = &files[*at];
        // A compiler that fails on a file decides nothing for it.
        crate::contained::contained(|| compile(file, source, *mutant))
            .ok()
            .flatten()
    });
    let mut compiled = compiled.into_iter();
    let mut duplicate_positions = Vec::new();
    let test_time = |plan: &StrongPlan, position: usize| {
        plan.mutants[position]
            .candidates
            .iter()
            .filter_map(|candidate| plan.tests.get(candidate.test)?.duration_ms)
            .sum::<u64>()
    };
    for (file, _, mutants) in &files {
        let original = compiled.next().expect("one compile per job");
        if original.is_none() {
            same.not_compiled.push(file.clone());
        }
        let mut first_with = std::collections::HashMap::<String, usize>::new();
        for (position, _) in mutants {
            let code = compiled.next().expect("one compile per job");
            let (Some(original), Some(code)) = (&original, code) else {
                continue;
            };
            let undecided = plan.mutants[*position].decided.is_none();
            if *original == code && !shared(&plan.mutants[*position].id) {
                if undecided {
                    same.saved_ms += test_time(plan, *position);
                    same.equivalent_to_run += 1;
                }
                let mutant = &mut plan.mutants[*position];
                mutant.decided = Some(Decided::Equivalent);
                mutant.reason = Some(COMPILES_TO_ORIGINAL.to_owned());
                mutant.candidates.clear();
                mutant.whole_files.clear();
                mutant.server_wide = None;
                same.equivalent += 1;
                continue;
            }
            if !undecided {
                continue;
            }
            match first_with.entry(code) {
                std::collections::hash_map::Entry::Vacant(entry) => {
                    entry.insert(*position);
                }
                // A verdict a mutant has is its own, whatever it compiles to.
                std::collections::hash_map::Entry::Occupied(_)
                    if shared(&plan.mutants[*position].id) => {}
                std::collections::hash_map::Entry::Occupied(entry) => {
                    let kept = &plan.mutants[*entry.get()].id;
                    same.duplicates
                        .insert(plan.mutants[*position].id.clone(), kept.clone());
                    same.saved_ms += test_time(plan, *position);
                    duplicate_positions.push(*position);
                }
            }
        }
    }
    duplicate_positions.sort_unstable();
    let mut position = 0;
    plan.mutants.retain(|_| {
        let keep = duplicate_positions.binary_search(&position).is_err();
        position += 1;
        keep
    });
    same
}

/// The verdicts `duplicates` take from the mutants that ran for them: each
/// one whose mutant has a verdict a run decides (killed, survived, timeout),
/// that verdict, run by none of its own tests.
pub fn shared_verdicts(
    duplicates: &BTreeMap<String, String>,
    verdicts: &BTreeMap<String, Verdict>,
) -> BTreeMap<String, Verdict> {
    duplicates
        .iter()
        .filter_map(|(duplicate, kept)| {
            let verdict = verdicts.get(kept)?;
            SHARED.contains(&verdict.status.as_str()).then(|| {
                (
                    duplicate.clone(),
                    Verdict {
                        status: verdict.status.clone(),
                        killed_by: verdict.killed_by.clone(),
                        tests: 0,
                        duration_ms: 0,
                        reason: Some(format!("{SAME_CODE_AS}{kept}")),
                    },
                )
            })
        })
        .collect()
}

/// `file`'s mutants by id, as the run lists them; `None` where it does not
/// parse.
fn listed_by_id(file: &str, source: &str) -> Option<BTreeMap<String, LedgerMutant>> {
    let allocator = Allocator::default();
    let source_type = crate::js_instrumenter::project_source_type(Path::new(file)).ok()?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.panicked || !parsed.errors.is_empty() {
        return None;
    }
    Some(
        crate::js_mutants::enumerate(&parsed.program, source)
            .into_iter()
            .map(|mutant| {
                let id = format!(
                    "{file}:{}:{}-{}:{}:{}:{}",
                    mutant.start.0,
                    mutant.start.1,
                    mutant.end.0,
                    mutant.end.1,
                    mutant.mutator,
                    mutant.replacement
                );
                (id, mutant)
            })
            .collect(),
    )
}

/// The options under which compiling preserves what a program does for
/// every input: no `@__PURE__` annotation trusted, every property read,
/// global read and import taken to have effects, function and class names
/// kept, nothing unreferenced dropped, `debugger` kept, and no name
/// mangled.
fn minifier_options() -> MinifierOptions {
    MinifierOptions {
        mangle: None,
        compress: Some(CompressOptions {
            keep_names: CompressOptionsKeepNames::all_true(),
            drop_debugger: false,
            drop_console: false,
            unused: CompressOptionsUnused::Keep,
            treeshake: TreeShakeOptions {
                annotations: false,
                invalid_import_side_effects: true,
                ..TreeShakeOptions::default()
            },
            ..CompressOptions::safest()
        }),
    }
}

/// `source` compiled with `mutant` applied (the original where `None`):
/// `None` where it does not parse, the mutant's node is not found, the
/// program has an early error, or the transform reports one.
pub(crate) fn compile(file: &str, source: &str, mutant: Option<&LedgerMutant>) -> Option<String> {
    let allocator = Allocator::default();
    let path = Path::new(file);
    let source_type = crate::js_instrumenter::project_source_type(path).ok()?;
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.panicked || !parsed.errors.is_empty() {
        return None;
    }
    let mut program = parsed.program;
    if let Some(mutant) = mutant
        && !crate::js_schemata::apply_in_place(&allocator, &mut program, mutant)
    {
        return None;
    }
    // `enum`s are evaluated as the transform needs them to erase them.
    let semantic = SemanticBuilder::new()
        .with_check_syntax_error(true)
        .with_enum_eval(true)
        .build(&program);
    if !semantic.errors.is_empty() {
        return None;
    }
    let scoping = semantic.semantic.into_scoping();
    let options = TransformOptions {
        // Every import kept as written: whether one is used is no reason to
        // drop it, and the two programs must keep the same ones.
        typescript: TypeScriptOptions {
            only_remove_type_imports: true,
            ..TypeScriptOptions::default()
        },
        ..TransformOptions::default()
    };
    let transformed =
        Transformer::new(&allocator, path, &options).build_with_scoping(scoping, &mut program);
    if !transformed.errors.is_empty() {
        return None;
    }
    Minifier::new(minifier_options()).minify(&allocator, &mut program);
    Some(
        Codegen::new()
            .with_options(CodegenOptions::minify())
            .build(&program)
            .code,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each of `source`'s mutants (as `src/a.ts` lists them), by
    /// `mutator:descriptor` at the text it mutates, and whether it compiles
    /// to the original.
    fn equivalent(file: &str, source: &str) -> Vec<(String, bool)> {
        let listed = listed_by_id(file, source).unwrap();
        let original = compile(file, source, None).unwrap();
        listed
            .values()
            .map(|mutant| {
                let text = &source[mutant.span.start as usize..mutant.span.end as usize];
                let code = compile(file, source, Some(mutant));
                (
                    format!("{text} {}:{}", mutant.mutator, mutant.replacement),
                    code.as_deref() == Some(original.as_str()),
                )
            })
            .collect()
    }

    fn equivalent_ones(file: &str, source: &str) -> Vec<String> {
        equivalent(file, source)
            .into_iter()
            .filter_map(|(name, same)| same.then_some(name))
            .collect()
    }

    #[test]
    fn dead_code_and_folded_constants_compile_to_the_original() {
        let source =
            "export function f(x) {\n  if (false) { console.log('never'); }\n  return x;\n}\n";
        assert_eq!(
            equivalent_ones("src/a.js", source),
            vec![
                "{ console.log('never'); } BlockStatement:{}".to_owned(),
                "'never' StringLiteral:empty".to_owned(),
                // `if (false)` made `if (false)`: the same code as written.
                "false ConditionalExpression:false".to_owned(),
            ]
        );
    }

    #[test]
    fn a_mutant_that_changes_what_the_program_does_does_not() {
        let source = "export const f = (a, b) => a < b ? a + b : 'x';\n";
        assert_eq!(equivalent_ones("src/a.ts", source), Vec::<String>::new());
        assert!(equivalent("src/a.ts", source).len() >= 5);
    }

    #[test]
    fn typescript_types_and_jsx_compile_away_alike() {
        let source = "type T = { a: number };\nexport const f = (t: T): number => t.a;\nexport const C = () => <div>{\"\"}</div>;\n";
        let compiled = compile("src/a.tsx", source, None).unwrap();
        assert!(!compiled.contains("number"), "{compiled}");
        assert!(!compiled.contains("<div"), "{compiled}");
        // `""` as a child renders nothing, like any empty string: a mutant
        // to `"Stryker was here!"` is not the same code.
        assert_eq!(equivalent_ones("src/a.tsx", source), Vec::<String>::new());
    }

    #[test]
    fn names_annotations_and_unused_code_are_kept() {
        // A function's name is observable (`f.name`), an unused function may
        // be read by `eval` or exported later, and a `/* @__PURE__ */` call
        // can still have effects: none is compiled away.
        let source = "export const f = function () { return 1; };\nfunction unused() { return 2; }\n/* @__PURE__ */ g(3);\n";
        let compiled = compile("src/a.js", source, None).unwrap();
        assert!(compiled.contains("unused"), "{compiled}");
        assert!(compiled.contains("g(3)"), "{compiled}");
        assert_eq!(equivalent_ones("src/a.js", source), Vec::<String>::new());
    }

    #[test]
    fn a_file_that_does_not_compile_decides_nothing() {
        assert_eq!(compile("src/a.js", "let = ;", None), None);
        assert_eq!(compile("src/a.unknown", "1", None), None);
        assert!(listed_by_id("src/a.js", "let = ;").is_none());
        assert!(listed_by_id("src/a.unknown", "1").is_none());
        // An early error the parser leaves to the semantic check.
        assert_eq!(compile("src/a.js", "let a; let a;", None), None);
        // A mutant whose node is not in the program.
        let elsewhere = LedgerMutant {
            span: oxc_span::Span::new(100, 101),
            start: (9, 1),
            end: (9, 2),
            mutator: "BooleanLiteral",
            replacement: "false".into(),
        };
        assert_eq!(compile("src/a.js", "true;", Some(&elsewhere)), None);
    }

    #[test]
    fn a_transform_error_decides_nothing() {
        // A JSX namespace name, which the JSX transform refuses.
        assert_eq!(
            compile("src/a.jsx", "export const a = <svg:rect />;\n", None),
            None
        );
    }

    fn plan_of(file: &str, source: &str) -> StrongPlan {
        let listed = listed_by_id(file, source).unwrap();
        StrongPlan {
            mutant_set: crate::js_mutants::MUTANT_SET.to_owned(),
            tests: Vec::new(),
            mutants: listed
                .keys()
                .enumerate()
                .map(|(index, id)| crate::strong_plan::PlannedMutant {
                    id: id.clone(),
                    file: file.to_owned(),
                    language: crate::mutant_list::JAVASCRIPT.to_owned(),
                    index,
                    mutator: id.rsplit(':').nth(1).unwrap().to_owned(),
                    weak: "infected",
                    decided: None,
                    reason: None,
                    candidates: vec![crate::strong_plan::Candidate {
                        test: 0,
                        reason: crate::strong_plan::CandidateReason::Infected,
                    }],
                    whole_files: Vec::new(),
                    server_wide: None,
                    server_ran: false,
                    passed_before: 0,
                    covering: 1,
                    statement: None,
                    function: None,
                    function_hit_by: BTreeMap::new(),
                    narrowed: None,
                })
                .collect(),
        }
    }

    #[test]
    fn a_plan_decides_equivalent_ones_and_runs_one_of_each_code() {
        let source = "export function f(x) {\n  if (false) { return 1; }\n  return x;\n}\n";
        let mut plan = plan_of("src/a.js", source);
        let before = plan.mutants.len();
        let same = decide_by_compiled_code(&mut plan, &BTreeMap::new(), |file| {
            (file == "src/a.js").then(|| source.to_owned())
        });
        let decided = plan
            .mutants
            .iter()
            .filter(|mutant| mutant.decided == Some(Decided::Equivalent))
            .map(|mutant| mutant.id.as_str())
            .collect::<Vec<_>>();
        // The dead branch emptied, and its test made what it is.
        assert_eq!(
            decided,
            [
                "src/a.js:2:14-2:27:BlockStatement:{}",
                "src/a.js:2:7-2:12:ConditionalExpression:false"
            ]
        );
        assert_eq!(same.equivalent, 2);
        assert!(
            plan.mutants
                .iter()
                .filter(|mutant| mutant.decided == Some(Decided::Equivalent))
                .all(
                    |mutant| mutant.reason.as_deref() == Some(COMPILES_TO_ORIGINAL)
                        && mutant.candidates.is_empty()
                )
        );
        // Every duplicate is out of the plan, and runs as a mutant still in it.
        assert_eq!(plan.mutants.len() + same.duplicates.len(), before);
        for (duplicate, kept) in &same.duplicates {
            assert!(plan.mutants.iter().all(|mutant| &mutant.id != duplicate));
            assert!(plan.mutants.iter().any(|mutant| &mutant.id == kept));
        }
    }

    #[test]
    fn duplicates_are_mutants_that_compile_alike() {
        // The body emptied, the `if`'s block emptied, its call removed and
        // its test made `false` all leave `function f(a) {}`: reading a
        // parameter does nothing.
        let source = "export function f(a) {\n  if (a) { g(); }\n}\n";
        let mut plan = plan_of("src/a.js", source);
        let same =
            decide_by_compiled_code(&mut plan, &BTreeMap::new(), |_| Some(source.to_owned()));
        assert_eq!(same.equivalent, 0);
        assert_eq!(
            same.duplicates,
            BTreeMap::from([
                (
                    "src/a.js:2:10-2:18:BlockStatement:{}".to_owned(),
                    "src/a.js:1:22-3:2:BlockStatement:{}".to_owned()
                ),
                (
                    "src/a.js:2:12-2:16:CallExpression:empty".to_owned(),
                    "src/a.js:1:22-3:2:BlockStatement:{}".to_owned()
                ),
                (
                    "src/a.js:2:7-2:8:ConditionalExpression:false".to_owned(),
                    "src/a.js:1:22-3:2:BlockStatement:{}".to_owned()
                ),
            ])
        );
    }

    #[test]
    fn a_mutant_with_a_verdict_is_the_one_its_duplicates_take_it_from() {
        // As in `duplicates_are_mutants_that_compile_alike`, four mutants
        // that leave `function f(a) {}`; the last two already have verdicts.
        let source = "export function f(a) {\n  if (a) { g(); }\n}\n";
        let verdict = |status: &str| Verdict {
            status: status.into(),
            killed_by: None,
            tests: 1,
            duration_ms: 1,
            reason: None,
        };
        let settled = BTreeMap::from([
            (
                "src/a.js:2:12-2:16:CallExpression:empty".to_owned(),
                verdict("killed"),
            ),
            (
                "src/a.js:2:7-2:8:ConditionalExpression:false".to_owned(),
                verdict("killed"),
            ),
            // An error decides nothing: such a mutant is not compared.
            (
                "src/a.js:2:7-2:8:ConditionalExpression:true".to_owned(),
                verdict("error"),
            ),
        ]);
        let mut plan = plan_of("src/a.js", source);
        let same = decide_by_compiled_code(&mut plan, &settled, |_| Some(source.to_owned()));
        assert_eq!(
            same.duplicates,
            BTreeMap::from([
                (
                    "src/a.js:1:22-3:2:BlockStatement:{}".to_owned(),
                    "src/a.js:2:12-2:16:CallExpression:empty".to_owned()
                ),
                (
                    "src/a.js:2:10-2:18:BlockStatement:{}".to_owned(),
                    "src/a.js:2:12-2:16:CallExpression:empty".to_owned()
                ),
            ])
        );
        let shared = shared_verdicts(&same.duplicates, &settled);
        assert_eq!(shared.len(), 2);
        assert!(shared.values().all(|verdict| verdict.status == "killed"));
    }

    #[test]
    fn files_it_cannot_read_and_decided_or_other_languages_mutants_are_left() {
        let source = "export function f(x) {\n  if (false) { return 1; }\n  return x;\n}\n";
        let mut plan = plan_of("src/a.js", source);
        let untouched = plan.clone();
        assert_eq!(
            decide_by_compiled_code(&mut plan, &BTreeMap::new(), |_| None),
            SameCode::default()
        );
        assert_eq!(plan, untouched);
        for mutant in &mut plan.mutants {
            mutant.language = "python".into();
        }
        let other = plan.clone();
        assert_eq!(
            decide_by_compiled_code(&mut plan, &BTreeMap::new(), |_| Some(source.to_owned())),
            SameCode::default()
        );
        assert_eq!(plan, other);
        // Mutants the plan decided are compared with the original, not with
        // each other: one that compiles to it is equivalent, not run either
        // way.
        let mut decided = untouched.clone();
        for mutant in &mut decided.mutants {
            mutant.decided = Some(Decided::NoCoverage);
        }
        let same =
            decide_by_compiled_code(&mut decided, &BTreeMap::new(), |_| Some(source.to_owned()));
        assert_eq!(
            (
                same.equivalent,
                same.equivalent_to_run,
                same.duplicates.len(),
                same.saved_ms
            ),
            (2, 0, 0, 0)
        );
        assert_eq!(decided.mutants.len(), untouched.mutants.len());
        assert_eq!(
            decided
                .mutants
                .iter()
                .filter(|mutant| mutant.decided == Some(Decided::NoCoverage))
                .count(),
            untouched.mutants.len() - 2
        );
        // Ones already equivalent are left as they are.
        let kept = decided.clone();
        for mutant in &mut decided.mutants {
            mutant.decided = Some(Decided::Equivalent);
        }
        let all = decided.clone();
        assert_eq!(
            decide_by_compiled_code(&mut decided, &BTreeMap::new(), |_| Some(source.to_owned())),
            SameCode::default()
        );
        assert_eq!(decided, all);
        drop(kept);
        // A file whose source no longer parses, and a mutant the source no
        // longer lists, are left to run.
        let mut broken = untouched.clone();
        assert_eq!(
            decide_by_compiled_code(&mut broken, &BTreeMap::new(), |_| Some(
                "let = ;".to_owned()
            )),
            SameCode {
                not_compiled: vec!["src/a.js".into()],
                ..SameCode::default()
            }
        );
        assert_eq!(broken, untouched);
        let mut renamed = untouched.clone();
        for mutant in &mut renamed.mutants {
            mutant.id.push('x');
        }
        let before = renamed.clone();
        decide_by_compiled_code(&mut renamed, &BTreeMap::new(), |_| Some(source.to_owned()));
        assert_eq!(renamed, before);
    }

    #[test]
    fn a_duplicate_takes_only_a_verdict_a_run_decides() {
        let verdict = |status: &str| Verdict {
            status: status.into(),
            killed_by: (status == "killed").then(|| "t1".to_owned()),
            tests: 3,
            duration_ms: 40,
            reason: None,
        };
        let duplicates = BTreeMap::from([
            ("d1".to_owned(), "k".to_owned()),
            ("d2".to_owned(), "s".to_owned()),
            ("d3".to_owned(), "t".to_owned()),
            ("d4".to_owned(), "e".to_owned()),
            ("d5".to_owned(), "none".to_owned()),
        ]);
        let verdicts = BTreeMap::from([
            ("k".to_owned(), verdict("killed")),
            ("s".to_owned(), verdict("survived")),
            ("t".to_owned(), verdict("timeout")),
            ("e".to_owned(), verdict("error")),
        ]);
        let shared = shared_verdicts(&duplicates, &verdicts);
        assert_eq!(
            shared.keys().map(String::as_str).collect::<Vec<_>>(),
            ["d1", "d2", "d3"]
        );
        assert_eq!(
            shared["d1"],
            Verdict {
                status: "killed".into(),
                killed_by: Some("t1".into()),
                tests: 0,
                duration_ms: 0,
                reason: Some("same-code-as:k".into()),
            }
        );
        assert_eq!(shared["d2"].status, "survived");
    }
}

/// How many of a tree's mutants compiling compares away, by mutator. On its
/// own it measures the StrykerJS parity fixtures; `SUPERCOV_SAME_CODE_SOURCES=
/// <dir>[:<dir>...] cargo test --release -p supercov-engine --lib
/// same_code_census -- --nocapture` measures other trees. Each source file
/// (`.ts`, `.tsx`, `.js`, `.jsx`, `.mts`, `.mjs`; not a test, a declaration
/// or a configuration) under the directories is listed and every mutant
/// compiled.
#[cfg(test)]
mod census {
    use super::*;

    const SOURCE: [&str; 6] = [".ts", ".tsx", ".js", ".jsx", ".mts", ".mjs"];
    const SKIPPED_DIRECTORIES: [&str; 7] = [
        "node_modules",
        "dist",
        ".git",
        "test",
        "tests",
        "__tests__",
        "e2e",
    ];

    fn is_source(name: &str) -> bool {
        SOURCE.iter().any(|extension| name.ends_with(extension))
            && ![".d.ts", ".test.", ".spec.", ".config."]
                .iter()
                .any(|part| name.contains(part))
    }

    fn sources(dir: &Path, into: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() && !SKIPPED_DIRECTORIES.contains(&name.as_str()) {
                sources(&path, into);
            } else if is_source(&name) {
                into.push(path);
            }
        }
    }

    /// Each mutant of a file's: its mutator, and whether its code is the
    /// original's (`equivalent`), another mutant's (`duplicate`), its own
    /// (`distinct`), or not compiled.
    fn census(
        file: &str,
        source: &str,
        mutants: &[LedgerMutant],
    ) -> Vec<(&'static str, &'static str)> {
        let original = compile(file, source, None);
        let mut seen = std::collections::HashSet::new();
        mutants
            .iter()
            .map(|mutant| {
                let code = compile(file, source, Some(mutant));
                let verdict = match code {
                    None => "uncompiled",
                    Some(code) if Some(&code) == original.as_ref() => "equivalent",
                    Some(code) if seen.contains(&code) => "duplicate",
                    Some(code) => {
                        seen.insert(code);
                        "distinct"
                    }
                };
                (mutant.mutator, verdict)
            })
            .collect()
    }

    #[test]
    fn same_code_census() {
        let parity = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/stryker-parity");
        let dirs = std::env::var("SUPERCOV_SAME_CODE_SOURCES").unwrap_or_else(|_| parity.into());
        let mut counted = BTreeMap::<&str, usize>::new();
        for dir in dirs.split(':') {
            let mut files = Vec::new();
            sources(Path::new(dir), &mut files);
            let jobs = files
                .iter()
                .filter_map(|path| {
                    let source = std::fs::read_to_string(path).ok()?;
                    let file = path.to_string_lossy().into_owned();
                    let listed = listed_by_id(&file, &source)?;
                    Some((file, source, listed.into_values().collect::<Vec<_>>()))
                })
                .collect::<Vec<_>>();
            let started = std::time::Instant::now();
            let per_file = crate::threads::parallel_map(&jobs, |(file, source, mutants)| {
                census(file, source, mutants)
            });
            let elapsed = started.elapsed();
            let mut by_mutator = BTreeMap::<&str, BTreeMap<&str, usize>>::new();
            let mut total = BTreeMap::<&str, usize>::new();
            for (mutator, verdict) in per_file.iter().flatten() {
                *by_mutator
                    .entry(mutator)
                    .or_default()
                    .entry(verdict)
                    .or_default() += 1;
                *total.entry(verdict).or_default() += 1;
                *counted.entry(verdict).or_default() += 1;
            }
            println!(
                "{dir}: {} files, {} mutants, {total:?} in {:.1} s",
                jobs.len(),
                total.values().sum::<usize>(),
                elapsed.as_secs_f64()
            );
            for (mutator, verdicts) in by_mutator {
                println!("  {mutator}: {verdicts:?}");
            }
        }
        assert!(
            counted.get("distinct").is_some_and(|n| *n > 0),
            "{counted:?}"
        );
    }
}
