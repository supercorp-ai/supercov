//! Grading the functions and methods inside an already assessed file.
//!
//! A file's grade says a file needs attention; it does not say where in the
//! file to look. Deepening asks Jev about each declaration on its own, with the
//! whole file supplied once as shared context so a declaration is read in the
//! place it actually lives rather than as a snippet.
//!
//! Only the file a scan already graded can be deepened, and only while the
//! working tree still holds the exact bytes that were graded. A grade that
//! described other source would be worse than no grade.
//!
//! Only readability and maintainability are graded here, and neither carries a
//! cutoff: the file cutoff was selected on whole human-rated classes, so nothing
//! at this scope is calibrated, and these grades order declarations and explain
//! themselves, nothing more.
//!
//! Correctness and failure handling were graded here until 2026-09-18 and were
//! withdrawn. Tested on twelve real upstream fixes, reformatting a file without
//! changing its behaviour moved those two constructs by a median of 0.16 and
//! 0.15 per declaration, while the fixes themselves moved the declaration they
//! repaired by a median of 0.13. A grade that answers more to whitespace than
//! to the defect is not evidence about the defect. The same reformatting moved
//! readability and maintainability by a median of 0.03, which is why they
//! stayed. Both remain file-scope questions, where they were measured to rise
//! for three of three independently reproduced fixes.

use super::*;

/// Judged alongside the four graded constructs: whether a declaration is
/// substantial enough for a separate judgment to say anything. A one-line
/// accessor scoring well is not news.
const SUBSTANCE: &str = "Is the declaration this question names substantial enough that judging it on its own says something, rather than being a trivial accessor, a constant, or a direct pass-through to something else?";

#[derive(Deserialize)]
struct Construct {
    id: String,
    #[serde(default)]
    statement: String,
    task: String,
    #[serde(default)]
    conditions: String,
    levels: Vec<String>,
}

fn constructs() -> Vec<Construct> {
    serde_json::from_str(include_str!("declarations.json")).expect("bundled declaration rubric")
}

/// One declaration worth grading on its own.
#[derive(Debug, Clone, Serialize)]
pub struct Declaration {
    pub name: String,
    pub kind: String,
    pub start_line: usize,
    pub end_line: usize,
}

impl Declaration {
    fn lines(&self) -> String {
        format!("{}-{}", self.start_line, self.end_line)
    }
}

/// Every function and method a file declares, excluding the closures nested
/// inside them: a callback is part of the code that installs it, and grading it
/// apart from that code would report noise rather than a place to look.
pub fn gradable(path: &str, source: &str) -> Option<Vec<Declaration>> {
    let code = supercov_engine::source_units::code(path, source)?;
    Some(
        code.units
            .iter()
            .enumerate()
            .filter(|(index, unit)| {
                unit.is_code()
                    && !unit.inert
                    && !code
                        .ancestors(*index)
                        .skip(1)
                        .any(|above| code.units[above].is_code())
            })
            .map(|(_, unit)| Declaration {
                name: unit.path.clone(),
                kind: unit.kind.clone(),
                start_line: unit.line,
                end_line: unit.end_line,
            })
            .collect(),
    )
}

/// What a snapshot records about a file: the declarations a later pass could
/// grade, or nothing at all when Supercov has no parser for the language.
pub fn inventory(path: &str, source: &str) -> Value {
    gradable(path, source)
        .and_then(|found| serde_json::to_value(found).ok())
        .unwrap_or(Value::Null)
}

fn questions(declarations: &[&Declaration]) -> Map<String, Value> {
    let mut questions = Map::new();
    for (index, declaration) in declarations.iter().enumerate() {
        // The question id is a routing key the model never sees, so every
        // question names its declaration itself.
        let named = json!({
            "declaration": declaration.name,
            "kind": declaration.kind,
            "lines": declaration.lines(),
        });
        for construct in constructs() {
            let mut instructions = named.clone();
            instructions["axis"] = json!(construct.id);
            if !construct.statement.is_empty() {
                instructions["statement"] = json!(construct.statement);
            }
            instructions["task"] = json!(construct.task);
            if !construct.conditions.is_empty() {
                instructions["grade_conditions"] = json!(construct.conditions);
            }
            questions.insert(
                format!("d{index}_{}_score", construct.id),
                json!({
                    "type": "score", "instructions": instructions, "criteria": construct.levels
                }),
            );
        }
        let mut instructions = named;
        instructions["task"] = json!(SUBSTANCE);
        questions.insert(
            format!("d{index}_substance"),
            json!({
                "type": "noul",
                "instructions": instructions,
                "criteria": {
                    "true": "It holds enough logic of its own that a separate judgment of it is informative.",
                    "false": "It is trivial or passes straight through to something else, so a separate judgment of it says little."
                }
            }),
        );
    }
    questions
}

fn request(
    path: &str,
    scope: &str,
    source: &str,
    declarations: &[&Declaration],
    context: Option<&str>,
) -> Value {
    let mut state = json!({
        "file": {"path": path, "supplied": scope, "source": source},
        "declarations": declarations.iter().map(|declaration| json!({
            "name": declaration.name, "kind": declaration.kind, "lines": declaration.lines(),
        })).collect::<Vec<_>>(),
    });
    if let Some(context) = context {
        state["context"] = json!({"contract": context});
    }
    json!({"model": MODEL, "state": state, "questions": questions(declarations)})
}

/// The text a declaration is judged inside: the whole file when a scan graded
/// it whole, otherwise the window that holds it, so a declaration is read in
/// the same context its file grade came from.
struct Supplied<'s> {
    scope: String,
    source: &'s str,
    start_line: usize,
    end_line: usize,
}

fn supplied<'s>(
    path: &str,
    source: &'s str,
    context: Option<&str>,
) -> Result<Vec<Supplied<'s>>, String> {
    let lines = line_starts(source);
    if within_budget(&super::request(path, source, context))?.is_some() {
        return Ok(vec![Supplied {
            scope: "the whole file".to_owned(),
            source,
            start_line: 1,
            end_line: lines.len(),
        }]);
    }
    let (_, planned) = plan(path, source, context)?;
    Ok(planned
        .iter()
        .filter(|(_, oversized)| !oversized)
        .map(|(window, _)| Supplied {
            scope: format!(
                "lines {}-{} of the file, the window this part of it was graded in",
                window.start_line, window.end_line
            ),
            source: window_source(source, &lines, window),
            start_line: window.start_line,
            end_line: window.end_line,
        })
        .collect())
}

/// Grade every declaration of one file, in as few requests as the budget
/// allows. Each request carries the shared source once and asks about the
/// declarations it can afford to ask about.
pub fn assess(
    path: &str,
    source: &str,
    context: Option<&str>,
    answer: &mut Answering<'_>,
) -> Result<Vec<Value>, String> {
    let declarations = gradable(path, source).unwrap_or_default();
    if declarations.is_empty() {
        return Err(format!(
            "{path} declares no function or method that can be assessed on its own"
        ));
    }
    let mut graded = Vec::new();
    for supplied in supplied(path, source, context)? {
        let mine: Vec<&Declaration> = declarations
            .iter()
            .filter(|declaration| {
                (supplied.start_line..=supplied.end_line).contains(&declaration.start_line)
            })
            .collect();
        let mut batch: Vec<&Declaration> = Vec::new();
        for declaration in mine {
            batch.push(declaration);
            let built = request(path, &supplied.scope, supplied.source, &batch, context);
            if within_budget(&built)?.is_some() {
                continue;
            }
            let moved = batch.pop().expect("the declaration just pushed");
            if batch.is_empty() {
                // The file alone leaves no room for even one declaration's
                // questions. Say so for this declaration and keep going.
                graded.push(json!({
                    "name": moved.name, "kind": moved.kind,
                    "start_line": moved.start_line, "end_line": moved.end_line,
                    "status": "error",
                    "error": format!("{path} leaves no room in one request for questions about {}", moved.name),
                }));
                continue;
            }
            graded.extend(send(path, &supplied, &batch, context, answer)?);
            batch = vec![moved];
        }
        if !batch.is_empty() {
            graded.extend(send(path, &supplied, &batch, context, answer)?);
        }
    }
    Ok(graded)
}

fn send(
    path: &str,
    supplied: &Supplied<'_>,
    batch: &[&Declaration],
    context: Option<&str>,
    answer: &mut Answering<'_>,
) -> Result<Vec<Value>, String> {
    let built = request(path, &supplied.scope, supplied.source, batch, context);
    let bytes = within_budget(&built)?
        .ok_or_else(|| format!("a batch of declarations for {path} is over the request budget"))?;
    let (hash, entry, hit, warning) = answer(&built, &bytes)?;
    Ok(batch
        .iter()
        .enumerate()
        .map(|(index, declaration)| {
            let dimensions: Map<String, Value> = constructs()
                .into_iter()
                .filter_map(|construct| {
                    let Answer::Score {
                        score, confidence, ..
                    } = entry
                        .response
                        .answers
                        .get(&format!("d{index}_{}_score", construct.id))?
                    else {
                        return None;
                    };
                    // No cutoff and no marker: the file cutoffs were selected on
                    // whole human-rated classes, and nothing at this scope is
                    // calibrated against anything.
                    Some((
                        construct.id,
                        json!({
                            "score": score / (construct.levels.len() - 1) as f64 * 10.0,
                            "confidence": confidence,
                        }),
                    ))
                })
                .collect();
            json!({
                "name": declaration.name, "kind": declaration.kind,
                "start_line": declaration.start_line, "end_line": declaration.end_line,
                "status": "completed", "graded_within": supplied.scope,
                "request_hash": hash, "question_index": index, "cached": hit,
                "substance": noul(&entry.response, &format!("d{index}_substance")),
                "dimensions": dimensions, "warning": warning,
            })
        })
        .collect())
}

/// The constructs this scope grades, in the order a report shows them.
pub fn order() -> Vec<String> {
    constructs()
        .into_iter()
        .map(|construct| construct.id)
        .collect()
}
