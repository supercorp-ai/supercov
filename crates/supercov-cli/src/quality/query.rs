//! Reading a saved quality snapshot.
//!
//! Nothing here contacts the provider, needs a credential, or produces a grade.
//! Every number shown was Jev's answer when the snapshot was written; this
//! module only selects, orders and explains. Ordering a windowed file by its
//! weakest window is navigation, not a judgment about the file.

use super::*;

/// How many rows a listing shows unless the caller says otherwise.
pub const DEFAULT_LIMIT: usize = 20;

/// The grade a construct has for a file, and the scope it came from. A file
/// assessed whole has one; a windowed file is represented by its weakest
/// window, named so the reader knows what they are looking at.
fn scope_score(file: &Value, id: &str) -> Option<(f64, String)> {
    if let Some(score) = file["dimensions"][id]["score"].as_f64() {
        return Some((score, "file".to_owned()));
    }
    file["windows"]
        .as_array()?
        .iter()
        .filter_map(|window| {
            Some((
                window["dimensions"][id]["score"].as_f64()?,
                format!(
                    "window {}/{}, lines {}-{}",
                    window["index"], window["of"], window["start_line"], window["end_line"]
                ),
            ))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
}

fn confidence_of(file: &Value, id: &str, scope: &str) -> Option<f64> {
    if scope == "file" {
        return file["dimensions"][id]["confidence"].as_f64();
    }
    file["windows"].as_array()?.iter().find_map(|window| {
        let label = format!(
            "window {}/{}, lines {}-{}",
            window["index"], window["of"], window["start_line"], window["end_line"]
        );
        (label == scope).then(|| window["dimensions"][id]["confidence"].as_f64())?
    })
}

fn marked(file: &Value, id: &str) -> bool {
    let windows = file["windows"].as_array().map_or(&[][..], Vec::as_slice);
    std::iter::once(file)
        .chain(windows)
        .any(|scope| scope["dimensions"][id]["review_recommended"] == true)
}

fn assessed(files: &[Value]) -> Vec<&Value> {
    files
        .iter()
        .filter(|file| file["status"] == "completed")
        .collect()
}

/// Weakest first on one construct, then on the rest of the ranking order, so
/// two files with the same grade still have a stable place.
fn ranked<'f>(files: &[&'f Value], first: &str) -> Vec<(&'f Value, f64, String)> {
    let mut rows: Vec<_> = files
        .iter()
        .filter_map(|file| scope_score(file, first).map(|(score, scope)| (*file, score, scope)))
        .collect();
    rows.sort_by(|a, b| {
        a.1.total_cmp(&b.1)
            .then_with(|| {
                RANKING
                    .iter()
                    .filter(|id| **id != first)
                    .map(|id| {
                        match (
                            scope_score(a.0, id).map(|s| s.0),
                            scope_score(b.0, id).map(|s| s.0),
                        ) {
                            (Some(a), Some(b)) => a.total_cmp(&b),
                            _ => std::cmp::Ordering::Equal,
                        }
                    })
                    .find(|ordering| ordering.is_ne())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.0["path"].as_str().cmp(&b.0["path"].as_str()))
    });
    rows
}

fn limited<T>(rows: Vec<T>, limit: usize) -> (usize, Vec<T>) {
    let total = rows.len();
    match limit {
        0 => (total, rows),
        limit => (total, rows.into_iter().take(limit).collect()),
    }
}

/// The saved provider answer behind one graded scope, when the response cache
/// still holds it. A snapshot stays readable without it; the level wording and
/// distributions simply go missing.
fn response(root: &Path, hash: &str) -> Option<ApiResponse> {
    let name = format!("{hash}.json");
    [
        store::responses(root).join(&name),
        store::legacy_response(root, hash),
    ]
    .iter()
    .find_map(|path| {
        let entry: CacheEntry = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        (entry.request_hash == hash).then_some(entry.response)
    })
}

/// The level Jev put most of its probability on, with the exact wording that
/// level was asked as. This is the model's own description of the code, which
/// a score alone does not carry.
fn modal_level(response: &ApiResponse, id: &str) -> Option<(String, String)> {
    let Answer::Score {
        legend,
        probabilities,
        ..
    } = response.answers.get(&format!("{id}_score"))?
    else {
        return None;
    };
    let (index, _) = probabilities
        .iter()
        .max_by(|a, b| a.1.total_cmp(b.1).then_with(|| b.0.cmp(a.0)))?;
    Some((index.clone(), legend.get(index)?.clone()))
}

fn open(root: &Path, snapshot: Option<&str>) -> Result<(String, Value, Vec<Value>), String> {
    let id = store::resolve(root, snapshot)?;
    let (manifest, files) = store::read(root, &id)?;
    let files = files["files"]
        .as_array()
        .ok_or("invalid snapshot file record")?
        .clone();
    Ok((id, manifest, files))
}

pub fn snapshots(root: &Path, limit: usize) -> Result<Value, String> {
    let rows: Vec<Value> = store::list(root)?
        .into_iter()
        .map(|(id, manifest)| {
            json!({
                "id": id,
                "created_at": manifest["created_at"],
                "paths": manifest["paths"],
                "model": manifest["model"],
                "rubric_version": manifest["rubric_version"],
                "counts": manifest["counts"],
            })
        })
        .collect();
    let (total, shown) = limited(rows, limit);
    Ok(json!({"view": "snapshots", "total": total, "shown": shown.len(), "snapshots": shown}))
}

pub fn show(root: &Path, snapshot: Option<&str>, limit: usize) -> Result<Value, String> {
    let (id, manifest, files) = open(root, snapshot)?;
    let completed = assessed(&files);
    let constructs: Vec<Value> = rubric()
        .into_iter()
        .map(|dimension| {
            let weakest = ranked(&completed, &dimension.id).into_iter().next();
            json!({
                "id": dimension.id,
                "review_below": dimension.review_below,
                "basis": dimension.cutoff_basis,
                "marked": completed.iter().filter(|file| marked(file, &dimension.id)).count(),
                "weakest": weakest.map(|(file, score, scope)| json!({
                    "path": file["path"], "score": score, "scope": scope
                })),
            })
        })
        .collect();
    let rows: Vec<Value> = ranked(&completed, RANKING[0])
        .into_iter()
        .map(|(file, score, scope)| {
            json!({
                "path": file["path"], "partial": file["partial"] == true,
                "maintainability": score, "scope": scope,
                "readability": scope_score(file, "readability").map(|s| s.0),
                "overall": scope_score(file, "overall").map(|s| s.0),
                "marked": rubric().iter().any(|d| marked(file, &d.id)),
            })
        })
        .collect();
    let (total, weakest_first) = limited(rows, limit);
    Ok(json!({
        "view": "show", "snapshot_id": id, "manifest": manifest,
        "constructs": constructs, "total_files": total,
        "shown": weakest_first.len(), "weakest_first": weakest_first,
        "errors": files.iter().filter(|file| file["status"] == "error")
            .map(|file| json!({"path": file["path"], "error": file["error"]}))
            .collect::<Vec<_>>(),
    }))
}

pub fn dimension(
    root: &Path,
    name: &str,
    snapshot: Option<&str>,
    limit: usize,
) -> Result<Value, String> {
    let dimension = rubric()
        .into_iter()
        .find(|dimension| dimension.id == name)
        .ok_or_else(|| {
            format!(
                "no quality construct {name}; the rubric grades: {}",
                rubric()
                    .iter()
                    .map(|d| d.id.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
    let (id, manifest, files) = open(root, snapshot)?;
    let completed = assessed(&files);
    let rows: Vec<Value> = ranked(&completed, &dimension.id)
        .into_iter()
        .map(|(file, score, scope)| {
            json!({
                "path": file["path"], "score": score, "scope": scope,
                "confidence": confidence_of(file, &dimension.id, &scope),
                "review_recommended": marked(file, &dimension.id),
            })
        })
        .collect();
    let (total, shown) = limited(rows, limit);
    Ok(json!({
        "view": "dimension", "snapshot_id": id,
        "model": manifest["model"], "rubric_version": manifest["rubric_version"],
        "construct": {
            "id": dimension.id, "review_below": dimension.review_below,
            "basis": dimension.cutoff_basis, "levels": dimension.levels.len(),
            "statement": dimension.statement,
        },
        "total": total, "shown": shown.len(), "files": shown,
    }))
}

pub fn file(root: &Path, path: &str, snapshot: Option<&str>) -> Result<Value, String> {
    let (id, manifest, files) = open(root, snapshot)?;
    let wanted = path.replace('\\', "/");
    let matched = files
        .iter()
        .find(|file| file["path"].as_str() == Some(wanted.as_str()))
        .or_else(|| {
            let mut suffixed = files.iter().filter(|file| {
                file["path"]
                    .as_str()
                    .is_some_and(|stored| stored.ends_with(&format!("/{wanted}")))
            });
            suffixed.next().filter(|_| suffixed.next().is_none())
        })
        .ok_or_else(|| {
            format!("{path} is not in snapshot {id}; list its files with: supercov quality show {id} --limit 0")
        })?;
    if matched["status"] == "error" {
        return Ok(json!({
            "view": "file", "snapshot_id": id, "path": matched["path"],
            "status": "error", "error": matched["error"],
        }));
    }
    let windows = matched["windows"].as_array().map_or(&[][..], Vec::as_slice);
    let scopes: Vec<&Value> = if windows.is_empty() {
        vec![matched]
    } else {
        windows.iter().collect()
    };
    let described: Vec<Value> = scopes
        .iter()
        .map(|scope| {
            if scope["status"] == "error" {
                return json!({
                    "scope": format!("window {}/{}", scope["index"], scope["of"]),
                    "start_line": scope["start_line"], "end_line": scope["end_line"],
                    "declarations": scope["declarations"],
                    "status": "error", "error": scope["error"],
                });
            }
            let hash = scope["request_hash"].as_str().unwrap_or_default();
            let answers = response(root, hash);
            let constructs: Vec<Value> = rubric()
                .into_iter()
                .map(|dimension| {
                    let graded = &scope["dimensions"][&dimension.id];
                    let level = answers
                        .as_ref()
                        .and_then(|answers| modal_level(answers, &dimension.id));
                    json!({
                        "id": dimension.id,
                        "score": graded["score"], "confidence": graded["confidence"],
                        "review_below": graded["review_below"],
                        "review_recommended": graded["review_recommended"],
                        "level_index": level.as_ref().map(|(index, _)| index.clone()),
                        "level": level.map(|(_, text)| text),
                    })
                })
                .collect();
            let mut value = json!({
                "scope": if windows.is_empty() { "file".to_owned() }
                    else { format!("window {}/{}", scope["index"], scope["of"]) },
                "request_hash": scope["request_hash"],
                "response_available": answers.is_some(),
                "overall_score": scope["overall_score"],
                "behavior_context_sufficiency": scope["behavior_context_sufficiency"],
                "constructs": constructs,
            });
            for carried in [
                "start_line",
                "end_line",
                "declarations",
                "window_sufficiency",
            ] {
                if !scope[carried].is_null() {
                    value[carried] = scope[carried].clone();
                }
            }
            value
        })
        .collect();
    Ok(json!({
        "view": "file", "snapshot_id": id,
        "model": manifest["model"], "rubric_version": manifest["rubric_version"],
        "path": matched["path"], "bytes": matched["bytes"],
        "source_hash": matched["source_hash"], "partial": matched["partial"] == true,
        "declarations": matched["declarations"], "scopes": described,
    }))
}

fn marker(construct: &Value) -> String {
    match construct["review_below"].as_f64() {
        Some(cutoff) if construct["review_recommended"] == true => {
            format!("  REVIEW (below {cutoff})")
        }
        _ => String::new(),
    }
}

fn number(value: &Value) -> String {
    value
        .as_f64()
        .map_or_else(|| "    —".to_owned(), |score| format!("{score:5.2}"))
}

/// One text rendering for every view, chosen by the tag the view carries.
pub fn render(view: &Value) -> String {
    match view["view"].as_str().unwrap_or_default() {
        "snapshots" => render_snapshots(view),
        "show" => render_show(view),
        "dimension" => render_dimension(view),
        "file" => render_file(view),
        other => format!("unknown quality view: {other}\n"),
    }
}

fn render_snapshots(view: &Value) -> String {
    let rows = view["snapshots"].as_array().map_or(&[][..], Vec::as_slice);
    if rows.is_empty() {
        return "No quality snapshots here yet. Assess some source: supercov quality <path>\n"
            .to_owned();
    }
    let mut text = format!(
        "{} quality snapshots, most recent first (showing {}).\n",
        view["total"], view["shown"]
    );
    for row in rows {
        let counts = &row["counts"];
        text.push_str(&format!(
            "\n{} — {}\n   {} assessed, {} windowed, {} errors; scanned {}\n",
            row["id"].as_str().unwrap_or("?"),
            row["created_at"].as_str().unwrap_or("?"),
            counts["assessed"],
            counts["partial"],
            counts["errors"],
            row["paths"].as_array().map_or(String::new(), |paths| paths
                .iter()
                .filter_map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(" ")),
        ));
    }
    text
}

fn render_show(view: &Value) -> String {
    let manifest = &view["manifest"];
    let counts = &manifest["counts"];
    let id = view["snapshot_id"].as_str().unwrap_or("?");
    let mut text = format!(
        "Quality snapshot {id} — {}\nRubric {}, model {}. Scanned: {}\n{} files assessed, {} of them as windows, {} errors. Fresh input tokens {}.\n",
        manifest["created_at"].as_str().unwrap_or("?"),
        manifest["rubric_version"].as_str().unwrap_or("?"),
        manifest["model"].as_str().unwrap_or("?"),
        manifest["paths"]
            .as_array()
            .map_or(String::new(), |paths| paths
                .iter()
                .filter_map(|p| p.as_str())
                .collect::<Vec<_>>()
                .join(" ")),
        counts["assessed"],
        counts["partial"],
        counts["errors"],
        manifest["usage_this_run"]["input_tokens"],
    );
    text.push_str("\nConstructs — every grade is Jev's own answer; markers are advisory:\n");
    for construct in view["constructs"].as_array().map_or(&[][..], Vec::as_slice) {
        let cutoff = match construct["review_below"].as_f64() {
            Some(cutoff) => format!("below {cutoff:<6}"),
            None => "no cutoff   ".to_owned(),
        };
        let weakest = match construct["weakest"].as_object() {
            Some(weakest) => format!(
                "weakest {} {}",
                weakest["path"].as_str().unwrap_or("?"),
                number(&weakest["score"]).trim_start(),
            ),
            None => String::new(),
        };
        text.push_str(&format!(
            "  {:<16} {cutoff}  {:>3} marked   {weakest}\n",
            construct["id"].as_str().unwrap_or("?"),
            construct["marked"],
        ));
    }
    text.push_str(&format!(
        "\nWeakest maintainability first ({} of {} files):\n",
        view["shown"], view["total_files"]
    ));
    for (position, row) in view["weakest_first"]
        .as_array()
        .map_or(&[][..], Vec::as_slice)
        .iter()
        .enumerate()
    {
        text.push_str(&format!(
            "  {:>2}. {} maint {} read {} overall {}{}\n",
            position + 1,
            row["path"].as_str().unwrap_or("?"),
            number(&row["maintainability"]),
            number(&row["readability"]),
            number(&row["overall"]),
            if row["partial"] == true {
                format!("  ({})", row["scope"].as_str().unwrap_or("windowed"))
            } else {
                String::new()
            },
        ));
    }
    for failure in view["errors"].as_array().map_or(&[][..], Vec::as_slice) {
        text.push_str(&format!(
            "\n{}: ERROR — {}\n",
            failure["path"].as_str().unwrap_or("?"),
            failure["error"].as_str().unwrap_or("unknown error"),
        ));
    }
    text.push_str(&format!(
        "\nGo deeper:\n  supercov quality dimension maintainability {id}\n  supercov quality file <path> {id}\n",
    ));
    text
}

fn render_dimension(view: &Value) -> String {
    let construct = &view["construct"];
    let cutoff = match construct["review_below"].as_f64() {
        Some(cutoff) => format!(
            "cutoff {cutoff} ({})",
            construct["basis"].as_str().unwrap_or("advisory")
        ),
        None => "no calibrated cutoff, so no markers".to_owned(),
    };
    let mut text = format!(
        "{} in snapshot {} — {cutoff}\n",
        construct["id"].as_str().unwrap_or("?"),
        view["snapshot_id"].as_str().unwrap_or("?"),
    );
    if let Some(statement) = construct["statement"].as_str().filter(|s| !s.is_empty()) {
        text.push_str(&format!("Jev judged agreement with: {statement}\n"));
    }
    text.push_str(&format!(
        "{} of {} files, weakest first.\n\n",
        view["shown"], view["total"]
    ));
    for row in view["files"].as_array().map_or(&[][..], Vec::as_slice) {
        let scope = row["scope"].as_str().unwrap_or("file");
        text.push_str(&format!(
            "  {}/10  confidence {}  {}{}{}\n",
            number(&row["score"]),
            number(&row["confidence"]).trim_start(),
            row["path"].as_str().unwrap_or("?"),
            if scope == "file" {
                String::new()
            } else {
                format!("  ({scope})")
            },
            if row["review_recommended"] == true {
                "  REVIEW"
            } else {
                ""
            },
        ));
    }
    text
}

fn render_file(view: &Value) -> String {
    let path = view["path"].as_str().unwrap_or("?");
    if view["status"] == "error" {
        return format!(
            "{path} in snapshot {}: ERROR — {}\n",
            view["snapshot_id"].as_str().unwrap_or("?"),
            view["error"].as_str().unwrap_or("unknown error"),
        );
    }
    let declarations = view["declarations"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let mut text = format!(
        "{path} in snapshot {} — {} bytes, {} top-level declarations, rubric {}\n",
        view["snapshot_id"].as_str().unwrap_or("?"),
        view["bytes"],
        declarations.len(),
        view["rubric_version"].as_str().unwrap_or("?"),
    );
    if view["partial"] == true {
        text.push_str(
            "Assessed as windows of whole declarations; this file has no whole-file grade.\n",
        );
    }
    for scope in view["scopes"].as_array().map_or(&[][..], Vec::as_slice) {
        let label = scope["scope"].as_str().unwrap_or("file");
        if scope["status"] == "error" {
            text.push_str(&format!(
                "\n{label}: ERROR — {}\n",
                scope["error"].as_str().unwrap_or("unknown error")
            ));
            continue;
        }
        text.push_str(&format!(
            "\n{}",
            if label == "file" { "whole file" } else { label }
        ));
        if !scope["start_line"].is_null() {
            text.push_str(&format!(
                ", lines {}-{}",
                scope["start_line"], scope["end_line"]
            ));
        }
        text.push_str(&format!(
            " — Jev overall {}/10\n",
            number(&scope["overall_score"]).trim_start()
        ));
        for construct in scope["constructs"]
            .as_array()
            .map_or(&[][..], Vec::as_slice)
        {
            text.push_str(&format!(
                "  {:<16} {}/10  confidence {}{}\n",
                construct["id"].as_str().unwrap_or("?"),
                number(&construct["score"]),
                number(&construct["confidence"]).trim_start(),
                marker(construct),
            ));
            if let Some(level) = construct["level"].as_str() {
                text.push_str(&format!("      Jev chose: {level}\n"));
            }
        }
        if scope["response_available"] == false {
            text.push_str(
                "  The saved provider answer is no longer cached, so level wording is missing.\n",
            );
        }
        if let Some(sufficiency) = scope["behavior_context_sufficiency"].as_f64() {
            text.push_str(&format!(
                "  behavioral context sufficiency {sufficiency:.2}/1 (Jev judgment)\n"
            ));
        }
        if let Some(sufficiency) = scope["window_sufficiency"].as_f64() {
            text.push_str(&format!(
                "  window sufficiency {sufficiency:.2}/1 (Jev judgment)\n"
            ));
        }
    }
    if !declarations.is_empty() {
        text.push_str("\nTop-level declarations, none assessed on its own:\n");
        for declaration in declarations {
            text.push_str(&format!(
                "  {} ({}) lines {}-{}\n",
                declaration["name"].as_str().unwrap_or("?"),
                declaration["kind"].as_str().unwrap_or("?"),
                declaration["start_line"],
                declaration["end_line"],
            ));
        }
    }
    text
}
