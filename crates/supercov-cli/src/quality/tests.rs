use super::*;
use std::{
    io::Write,
    net::TcpListener,
    sync::atomic::{AtomicUsize, Ordering},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "supercov-quality-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn write(&self, path: &str, text: &str) {
        let target = self.0.join(path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, text).unwrap();
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn response(request: &Value) -> ApiResponse {
    let answers = request["questions"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, question)| {
            let answer = match question["type"].as_str().unwrap() {
                "noul" => Answer::Noul { noul: 0.99 },
                "choice" => {
                    let options = question["criteria"].as_object().unwrap();
                    let chosen = options.keys().next().unwrap().clone();
                    Answer::Choice {
                        confidence: 1.0,
                        probabilities: options
                            .keys()
                            .map(|option| (option.clone(), f64::from(*option == chosen)))
                            .collect(),
                        choice: chosen,
                    }
                }
                "score" => {
                    let levels = question["criteria"].as_array().unwrap();
                    Answer::Score {
                        score: (levels.len() - 1) as f64,
                        confidence: 1.0,
                        legend: levels
                            .iter()
                            .enumerate()
                            .map(|(i, v)| (i.to_string(), v.as_str().unwrap().into()))
                            .collect(),
                        probabilities: (0..levels.len())
                            .map(|i| (i.to_string(), if i == levels.len() - 1 { 1.0 } else { 0.0 }))
                            .collect(),
                    }
                }
                _ => unreachable!(),
            };
            (id.clone(), answer)
        })
        .collect();
    ApiResponse {
        model: MODEL.into(),
        answers,
        usage: Usage {
            input_tokens: 2345,
            output_tokens: 321,
        },
    }
}

#[test]
fn request_contains_source_once_and_explicit_targets() {
    let source = "// unique-comment\nexport const uniqueName = 42;";
    let req = request("src/example.ts", source, Some("A supplied contract"));
    assert_eq!(req["state"]["file"]["source"], source);
    assert_eq!(req["questions"].as_object().unwrap().len(), 10);
    assert_eq!(req.to_string().matches("uniqueName").count(), 1);
    for dimension in rubric() {
        let question = &req["questions"][format!("{}_score", dimension.id)];
        assert_eq!(question["instructions"]["axis"], dimension.id);
        assert_eq!(
            question["criteria"].as_array().unwrap().len(),
            dimension.levels.len()
        );
        let task = question["instructions"]["task"].as_str().unwrap();
        if dimension.task.is_empty() {
            assert_eq!(task, GRADING_TASK);
        } else {
            assert_eq!(task, dimension.task);
        }
        // The two constructs written against human rating instruments carry
        // their statement; the others must not invent one.
        assert_eq!(
            question["instructions"]["statement"].as_str(),
            (!dimension.statement.is_empty()).then_some(dimension.statement.as_str())
        );
    }
    assert_eq!(
        req["questions"]["readability_score"]["instructions"]["statement"],
        "This code is easy to read."
    );
    assert_eq!(
        req["questions"]["maintainability_score"]["instructions"]["statement"],
        "Overall, this code is maintainable."
    );
    let hash = digest(&serde_json::to_vec(&req).unwrap());
    assert_ne!(
        hash,
        digest(
            &serde_json::to_vec(&request(
                "src/example.ts",
                &source.replace("unique-comment", "edited-comment"),
                Some("A supplied contract")
            ))
            .unwrap()
        )
    );
    assert_ne!(
        hash,
        digest(
            &serde_json::to_vec(&request(
                "src/example.ts",
                source,
                Some("Different contract")
            ))
            .unwrap()
        )
    );
    assert!(within_budget(&req).unwrap().is_some());
}

#[test]
fn malformed_or_inconsistent_responses_are_rejected() {
    let req = request("a.ts", "export const a = 1;", None);
    let valid = response(&req);
    validate(&valid, &req).unwrap();
    let mut invalid = valid.clone();
    invalid.model = "jev-latest".into();
    assert!(validate(&invalid, &req).is_err());
    let mut invalid = valid.clone();
    invalid.answers.remove("readability_score");
    assert!(validate(&invalid, &req).is_err());
    let mut invalid = valid.clone();
    if let Answer::Score { score, .. } = invalid.answers.get_mut("readability_score").unwrap() {
        *score = 0.5;
    }
    assert!(validate(&invalid, &req).is_err());
    let mut invalid = valid.clone();
    invalid
        .answers
        .insert("behavior_context".into(), Answer::Noul { noul: 1.1 });
    assert!(validate(&invalid, &req).is_err());
    let mut invalid = valid;
    if let Answer::Score { probabilities, .. } = invalid.answers.get_mut("cohesion_score").unwrap()
    {
        probabilities.insert("invented".into(), 0.0);
    }
    assert!(validate(&invalid, &req).is_err());
}

#[test]
fn accepts_independently_rounded_live_scores_and_probabilities() {
    let req = request("a.js", "export const a = 1;", None);
    let mut actual = response(&req);
    if let Answer::Score {
        score,
        probabilities,
        confidence,
        ..
    } = actual.answers.get_mut("cohesion_score").unwrap()
    {
        // Observed from Jev 1.13.0 on bin/native.js: the rounded
        // distribution has a mean of 1.92, while the score is 1.91.
        *score = 1.91;
        *confidence = 0.87;
        *probabilities = BTreeMap::from([
            ("0".into(), 0.0),
            ("1".into(), 0.08),
            ("2".into(), 0.92),
            ("3".into(), 0.0),
            ("4".into(), 0.0),
        ]);
    }
    validate(&actual, &req).unwrap();
    if let Answer::Score {
        score,
        probabilities,
        ..
    } = actual.answers.get_mut("cohesion_score").unwrap()
    {
        *score = 1.88;
        *probabilities = BTreeMap::from([
            ("0".into(), 0.0),
            ("1".into(), 0.11),
            ("2".into(), 0.88),
            ("3".into(), 0.0),
            ("4".into(), 0.0),
        ]);
    }
    validate(&actual, &req).unwrap();
    if let Answer::Score { score, .. } = actual.answers.get_mut("cohesion_score").unwrap() {
        *score = 1.5;
    }
    assert!(validate(&actual, &req).is_err());
}

#[test]
fn context_and_confidence_do_not_suppress_or_change_jev_grades() {
    let req = request("a.rs", "fn a() {}", None);
    let mut res = response(&req);
    res.answers
        .insert("behavior_context".into(), Answer::Noul { noul: 0.1 });
    if let Answer::Score { confidence, .. } = res.answers.get_mut("cohesion_score").unwrap() {
        *confidence = 0.2;
    }
    validate(&res, &req).unwrap();
    let assessments = assess(&res, Some(5.0));
    assert_eq!(noul(&res, "behavior_context"), Some(0.1));
    assert_eq!(assessments["cohesion"].score, 10.0);
    assert_eq!(assessments["cohesion"].confidence, 0.2);
    assert!(!assessments["cohesion"].review_recommended);
}

#[test]
fn review_cutoff_is_strict_and_never_changes_the_grade() {
    let req = request("a.rs", "fn a() {}", None);
    let mut res = response(&req);
    if let Answer::Score {
        score,
        probabilities,
        ..
    } = res.answers.get_mut("correctness_score").unwrap()
    {
        *score = 2.0;
        for (id, p) in probabilities {
            *p = if id == "2" { 1.0 } else { 0.0 };
        }
    }
    validate(&res, &req).unwrap();
    assert!(!assess(&res, Some(5.0))["correctness"].review_recommended);
    assert!(assess(&res, Some(5.1))["correctness"].review_recommended);
    assert_eq!(assess(&res, Some(5.1))["correctness"].score, 5.0);
    assert_eq!(assess(&res, Some(5.1))["overall"].score, 10.0);
    // Correctness has no calibrated cutoff, so it carries no marker by default.
    assert!(!assess(&res, None)["correctness"].review_recommended);
    assert_eq!(assess(&res, None)["correctness"].review_below, None);
}

#[test]
fn discovery_honors_ignores_deduplicates_and_checks_scope() {
    let temp = Temp::new();
    temp.write(".gitignore", "ignored.ts\n");
    temp.write("src/a.ts", "export const a = 1;");
    temp.write("src/.hidden.ts", "hidden");
    temp.write("src/ignored.ts", "ignored");
    temp.write("src/node_modules/lib/index.js", "dependency");
    temp.write("src/target/lib.rs", "build");
    temp.write("src/readme.md", "docs");
    assert_eq!(
        discover(&temp.0, &["src".into(), "src/a.ts".into()]).unwrap(),
        vec![temp.0.join("src/a.ts")]
    );
    assert_eq!(
        discover(&temp.0, &["src/ignored.ts".into()]).unwrap(),
        vec![temp.0.join("src/ignored.ts")]
    );
    assert!(discover(&temp.0, &["..".into()]).is_err());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(temp.0.join("src/a.ts"), temp.0.join("link.ts")).unwrap();
        assert!(discover(&temp.0, &["link.ts".into()]).is_err());
        std::os::unix::fs::symlink(temp.0.join("src"), temp.0.join("linked-dir")).unwrap();
        assert!(discover(&temp.0, &["linked-dir/a.ts".into()]).is_err());
    }
}

/// A file well past the request budget, built from whole exported functions so
/// it has real declaration boundaries to window on.
fn oversized_typescript() -> String {
    let mut source = String::from(
        "// A preamble above the first declaration.\nimport { helper } from \"./helper\";\n\n",
    );
    for index in 0..40 {
        source.push_str(&format!(
            "export function step{index}(input: string): string {{\n{}  return helper(input) + \"{index}\";\n}}\n\n",
            format!("  // a line of body that exists only to take up room in step {index}\n").repeat(40),
        ));
    }
    source
}

#[test]
fn dry_run_needs_no_key_writes_nothing_and_plans_windows() {
    let temp = Temp::new();
    temp.write("ok.ts", "export const ok = true;");
    temp.write("big.ts", &oversized_typescript());
    // Over budget, and Supercov has no declaration parser for C#.
    temp.write("big.cs", &"// a filler comment line\n".repeat(6_000));
    let options = Options {
        paths: vec![".".into()],
        dry_run: true,
        ..Default::default()
    };
    let (report, errors) = run(&temp.0, &options, None).unwrap();
    assert!(errors);
    let requests = report["requests"].as_array().unwrap();
    assert_eq!(requests.iter().filter(|r| r["path"] == "ok.ts").count(), 1);
    assert!(requests.iter().filter(|r| r["path"] == "big.ts").count() >= 2);
    assert!(requests.iter().all(|r| r["request"]["model"] == MODEL));
    // The windowed requests carry their window and the file outline.
    let window = requests
        .iter()
        .find(|r| r["path"] == "big.ts")
        .expect("a windowed request");
    assert_eq!(window["request"]["state"]["file"]["window"]["index"], 1);
    assert!(
        window["request"]["state"]["outline"]
            .as_array()
            .unwrap()
            .len()
            >= 40
    );
    assert!(
        window["request"]["questions"]["readability_score"]["instructions"]["scope"]
            .as_str()
            .unwrap()
            .contains("state.file.window.source")
    );
    assert_eq!(
        window["request"]["questions"].as_object().unwrap().len(),
        11
    );
    let failures = report["errors"].as_array().unwrap();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0]["path"], "big.cs");
    assert!(
        failures[0]["error"]
            .as_str()
            .unwrap()
            .contains("no parsed top-level declarations")
    );
    assert!(!temp.0.join(".supercov").exists());
}

#[test]
fn windows_partition_the_file_at_declaration_boundaries_and_each_one_fits() {
    let source = oversized_typescript();
    let starts = line_starts(&source);
    let (outline, planned) = plan("src/big.ts", &source, None).unwrap();
    assert!(
        planned.len() >= 2,
        "an over-budget file needs several windows"
    );
    assert!(planned.iter().all(|(_, oversized)| !oversized));
    assert_eq!(
        planned[0].0.start_line, 1,
        "the preamble joins the first window"
    );
    for pair in planned.windows(2) {
        assert_eq!(pair[1].0.start_line, pair[0].0.end_line + 1);
    }
    assert_eq!(planned.last().unwrap().0.end_line, starts.len());
    // Every window fits its own request, and the windows reassemble the exact
    // source: no byte is sent twice and none is dropped.
    let mut rebuilt = String::new();
    for (index, (window, _)) in planned.iter().enumerate() {
        assert_eq!(window.index, index + 1);
        assert_eq!(window.of, planned.len());
        let text = window_source(&source, &starts, window);
        rebuilt.push_str(text);
        let request = windowed_request("src/big.ts", text, window, &outline, None);
        assert!(within_budget(&request).unwrap().is_some());
        assert_eq!(request["state"]["file"]["window"]["source"], text);
    }
    assert_eq!(rebuilt, source);
    // Each declaration is named by exactly one window.
    let named: usize = planned
        .iter()
        .map(|(window, _)| window.declarations.len())
        .sum();
    assert_eq!(named, outline.as_array().unwrap().len());
    // A window boundary only ever falls on the first line of a declaration.
    let boundaries: BTreeSet<usize> = outline
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| unit["start_line"].as_u64().unwrap() as usize)
        .collect();
    for (window, _) in planned.iter().skip(1) {
        assert!(boundaries.contains(&window.start_line));
    }
}

#[test]
fn one_declaration_over_budget_is_flagged_without_losing_its_neighbours() {
    let body = "  // one line of a body far too large to send in a single request\n".repeat(2_000);
    // Plain constants are not declarations, so the neighbours here are
    // functions: only a declaration boundary can start a window.
    let source = format!(
        "export function head() {{\n  return 1;\n}}\nexport function huge() {{\n{body}}}\nexport function tail() {{\n  return 2;\n}}\n"
    );
    let (_, planned) = plan("src/big.ts", &source, None).unwrap();
    let flagged: Vec<_> = planned
        .iter()
        .filter(|(_, oversized)| *oversized)
        .flat_map(|(window, _)| window.declarations.clone())
        .collect();
    assert_eq!(flagged, vec!["huge".to_owned()]);
    let assessable: Vec<_> = planned
        .iter()
        .filter(|(_, oversized)| !*oversized)
        .flat_map(|(window, _)| window.declarations.clone())
        .collect();
    assert_eq!(assessable, vec!["head".to_owned(), "tail".to_owned()]);
}

#[test]
fn a_windowed_report_has_no_whole_file_grade_and_ranks_by_its_weakest_window() {
    let report = json!({
        "rubric_version": RUBRIC_VERSION, "model": MODEL,
        "usage_this_run": {"input_tokens": 0},
        "files": [
            {"path": "small.ts", "status": "completed", "overall_score": 8.0, "cached": false,
             "behavior_context_sufficiency": 0.9,
             "dimensions": {"maintainability": {"score": 8.0, "confidence": 0.8, "review_below": 6.9, "review_recommended": false}}},
            {"path": "big.ts", "status": "completed", "partial": true,
             "windows_weakest": {"maintainability": 3.0},
             "windows": [
                {"index": 1, "of": 2, "start_line": 1, "end_line": 120, "declarations": ["alpha"],
                 "status": "completed", "overall_score": 7.0, "cached": false, "window_sufficiency": 0.4,
                 "dimensions": {"maintainability": {"score": 7.0, "confidence": 0.5, "review_below": 6.9, "review_recommended": false}}},
                {"index": 2, "of": 2, "start_line": 121, "end_line": 240, "declarations": ["beta"],
                 "status": "error", "error": "lines 121-240 hold beta, which is over the request budget on its own"},
             ]},
        ]
    });
    let text = human(&report);
    assert!(text.find("big.ts").unwrap() < text.find("small.ts").unwrap());
    assert!(text.contains("2 windows of whole declarations, no whole-file grade"));
    assert!(text.contains("window 1/2 lines 1-120 (alpha) — maintainability 7.00/10"));
    assert!(text.contains("window sufficiency 0.40/1"));
    assert!(text.contains("window 2/2 lines 121-240 (beta): ERROR"));
    // The windowed file shows no overall grade line of its own.
    assert!(!text.contains("big.ts — maintainability"));
}

#[test]
fn cache_reuse_and_comment_invalidation_without_network() {
    let temp = Temp::new();
    let source = "// original\nexport const a = 1;";
    temp.write("a.ts", source);
    let req = request("a.ts", source, None);
    let hash = digest(&serde_json::to_vec(&req).unwrap());
    let cache = temp
        .0
        .join(".supercov/quality")
        .join(format!("{hash}.json"));
    let entry = CacheEntry {
        request_hash: hash.clone(),
        response: response(&req),
        elapsed_ms: 20,
    };
    save(&cache, &entry).unwrap();
    let options = Options {
        paths: vec!["a.ts".into()],
        json: true,
        ..Default::default()
    };
    let (report, errors) = run(&temp.0, &options, None).unwrap();
    assert!(!errors);
    assert_eq!(report["files"][0]["cached"], true);
    assert_eq!(report["files"][0]["overall_score"], 10.0);
    assert_eq!(report["usage_this_run"]["input_tokens"], 0);
    assert!(human(&report).contains("maintainability 10.00/10 (cached)"));
    assert!(report["files"][0].get("maintainability_index").is_none());
    let mut revised = entry;
    for (id, level) in [("overall_score", "1"), ("maintainability_score", "1")] {
        if let Answer::Score {
            score,
            probabilities,
            ..
        } = revised.response.answers.get_mut(id).unwrap()
        {
            *score = 1.0;
            for (index, p) in probabilities {
                *p = if index == level { 1.0 } else { 0.0 };
            }
        }
    }
    save(&cache, &revised).unwrap();
    let (report, errors) = run(&temp.0, &options, None).unwrap();
    assert!(!errors);
    assert_eq!(report["files"][0]["overall_score"], 2.5);
    assert_eq!(
        report["files"][0]["dimensions"]["readability"]["score"],
        10.0
    );
    // Jev's overall answer carries no calibrated cutoff; maintainability does.
    assert_eq!(
        report["files"][0]["dimensions"]["overall"]["review_recommended"],
        false
    );
    assert_eq!(
        report["files"][0]["dimensions"]["maintainability"]["review_recommended"],
        true
    );
    assert!(human(&report).contains("REVIEW (below 6.9)"));
    assert!(human(&report).contains("1 of 1 assessed files carry a review marker"));
    let changed_policy = Options {
        review_below: Some(2.0),
        ..options
    };
    let (report, errors) = run(&temp.0, &changed_policy, None).unwrap();
    assert!(!errors);
    assert_eq!(report["files"][0]["cached"], true);
    assert_eq!(
        report["files"][0]["dimensions"]["maintainability"]["review_recommended"],
        false
    );
    assert_eq!(report["policy"]["review_below_override"], 2.0);
    let options = changed_policy;
    temp.write("a.ts", &source.replace("original", "changed"));
    let (report, errors) = run(&temp.0, &options, None).unwrap();
    assert!(errors);
    assert!(
        report["files"][0]["error"]
            .as_str()
            .unwrap()
            .contains("TYPESAFE_API_KEY")
    );
    fs::write(&cache, "broken").unwrap();
    assert!(cached(&cache, &hash, &req).is_err());
}

// A real local HTTP exchange exercises serialization, authentication, response
// decoding and bounded retries without sending source to an external service.
fn server(req: Value, statuses: Vec<u16>) -> (String, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1/systemone", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        for status in statuses {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            let header = String::from_utf8(header).unwrap().to_lowercase();
            assert!(header.starts_with("post /v1/systemone "));
            assert!(header.contains("authorization: bearer test-key\r\n"));
            let length: usize = header
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            let mut body = vec![0; length];
            stream.read_exact(&mut body).unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), req);
            let body = if status == 200 {
                serde_json::to_string(&response(&req)).unwrap()
            } else {
                "private provider diagnostic".into()
            };
            write!(stream, "HTTP/1.1 {status} Mock\r\nContent-Type: application/json\r\nContent-Length: {}\r\nRetry-After: 0\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
    });
    (url, thread)
}

#[test]
fn http_roundtrip_retries_rate_limits_and_server_errors() {
    let req = request("a.py", "def a():\n    return 1\n", None);
    let (url, server) = server(req.clone(), vec![429, 529, 200]);
    let actual = evaluate(&client(), &url, "test-key", &req).unwrap();
    server.join().unwrap();
    assert_eq!(actual.usage.input_tokens, 2345);
    assert_eq!(assess(&actual, None)["readability"].score, 10.0);
}

#[test]
fn authentication_error_is_not_retried_or_echoed() {
    let req = request("a.py", "a = 1", None);
    let (url, server) = server(req.clone(), vec![401]);
    let error = evaluate(&client(), &url, "test-key", &req).unwrap_err();
    server.join().unwrap();
    assert!(error.contains("401"));
    assert!(!error.contains("test-key"));
    assert!(!error.contains("private provider"));
}

#[test]
fn scan_rejects_unknown_or_missing_options() {
    for value in ["NaN", "inf", "-1", "10.1", "abc"] {
        assert!(parse_scan(vec!["a.ts".into(), "--review-below".into(), value.into()]).is_err());
    }
    assert!(parse_scan(vec!["a.ts".into(), "--review-below".into()]).is_err());
    assert_eq!(
        parse_scan(vec!["a.ts".into(), "--review-below".into(), "7.5".into()])
            .unwrap()
            .review_below,
        Some(7.5)
    );
    assert!(
        parse_scan(vec![
            "a.ts".into(),
            "--review-below".into(),
            "5".into(),
            "--review-below".into(),
            "6".into()
        ])
        .is_err()
    );

    // No path is no longer an error; the caller fills in this directory.
    assert!(parse_scan(vec![]).unwrap().paths.is_empty());
    assert_eq!(defaulted(parse_scan(vec![]).unwrap()).paths, here());
    // A path given explicitly is never replaced by the default.
    assert_eq!(
        defaulted(parse_scan(vec!["a.ts".into()]).unwrap()).paths,
        vec![PathBuf::from("a.ts")]
    );
    assert!(parse_scan(vec!["a.ts".into(), "--context".into()]).is_err());
    assert!(parse_scan(vec!["a.ts".into(), "--scope".into(), "function".into()]).is_err());
    assert!(
        parse_scan(vec!["a.ts".into(), "--json".into(), "--dry-run".into()])
            .unwrap()
            .dry_run
    );
}

#[test]
fn cutoffs_are_per_construct_and_an_override_reaches_every_construct() {
    let req = request("a.ts", "export const a = 1;", None);
    let mut res = response(&req);
    for dimension in rubric() {
        if let Answer::Score {
            score,
            probabilities,
            ..
        } = res
            .answers
            .get_mut(&format!("{}_score", dimension.id))
            .unwrap()
        {
            *score = 0.0;
            for (index, p) in probabilities {
                *p = if index == "0" { 1.0 } else { 0.0 };
            }
        }
    }
    validate(&res, &req).unwrap();
    let assessments = assess(&res, None);
    for dimension in rubric() {
        let assessment = &assessments[&dimension.id];
        assert_eq!(assessment.score, 0.0);
        assert_eq!(assessment.review_below, dimension.review_below);
        assert_eq!(
            assessment.review_recommended,
            dimension.review_below.is_some()
        );
    }
    // Maintainability is the only construct with a cutoff that survived being
    // tested on classes no rubric had graded; every other construct, readability
    // included, is ranked but never marked.
    assert_eq!(assessments["maintainability"].review_below, Some(6.9));
    for id in [
        "readability",
        "correctness",
        "cohesion",
        "changeability",
        "overall",
    ] {
        assert_eq!(assessments[id].review_below, None);
        assert!(!assessments[id].review_recommended);
    }
    let overridden = assess(&res, Some(1.0));
    assert!(rubric().iter().all(
        |d| overridden[&d.id].review_below == Some(1.0) && overridden[&d.id].review_recommended
    ));
}

#[test]
fn each_construct_normalizes_on_its_own_level_count() {
    let req = request("a.ts", "export const a = 1;", None);
    let mut res = response(&req);
    assert_eq!(assess(&res, None)["maintainability"].score, 10.0);
    assert_eq!(assess(&res, None)["overall"].score, 10.0);
    // One level below the top differs between the four- and five-level scales.
    for (id, index) in [("maintainability_score", 2.0), ("overall_score", 3.0)] {
        if let Answer::Score {
            score,
            probabilities,
            ..
        } = res.answers.get_mut(id).unwrap()
        {
            *score = index;
            for (level, p) in probabilities {
                *p = if *level == (index as usize).to_string() {
                    1.0
                } else {
                    0.0
                };
            }
        }
    }
    validate(&res, &req).unwrap();
    let assessments = assess(&res, None);
    assert!((assessments["maintainability"].score - 20.0 / 3.0).abs() < 1e-9);
    assert_eq!(assessments["overall"].score, 7.5);
}

#[test]
fn files_are_ranked_weakest_maintainability_first() {
    let report = json!({
        "rubric_version": RUBRIC_VERSION, "model": MODEL,
        "usage_this_run": {"input_tokens": 0},
        "files": [
            {"path": "strong.ts", "status": "completed", "overall_score": 9.0, "cached": false,
             "behavior_context_sufficiency": 0.9,
             "dimensions": {"maintainability": {"score": 9.0, "confidence": 0.8, "review_below": 6.9, "review_recommended": false}}},
            {"path": "broken.ts", "status": "error", "error": "empty source file cannot be assessed"},
            {"path": "weak.ts", "status": "completed", "overall_score": 3.0, "cached": false,
             "behavior_context_sufficiency": 0.7,
             "dimensions": {"maintainability": {"score": 2.0, "confidence": 0.6, "review_below": 6.9, "review_recommended": true}}},
        ]
    });
    let text = human(&report);
    let weak = text.find("weak.ts").unwrap();
    let strong = text.find("strong.ts").unwrap();
    let error = text.find("broken.ts").unwrap();
    assert!(weak < strong, "weakest maintainability comes first");
    assert!(strong < error, "errors follow the ranked files");
    assert!(text.contains("1 of 2 assessed files carry a review marker"));
}

#[test]
fn a_subcommand_is_only_a_reserved_word_and_a_path_can_still_be_scanned() {
    let scanned =
        |arguments: Vec<&str>| match parse(arguments.iter().map(|a| a.to_string()).collect()) {
            Ok(Command::Health(options)) => options.paths,
            _ => panic!("expected an assessment"),
        };
    assert_eq!(scanned(vec!["src"]), vec![PathBuf::from("src")]);
    // No path at all is the repository. `supercov quality` should say something
    // about the code you are standing in without being told where to look.
    assert_eq!(scanned(vec![]), vec![PathBuf::from(".")]);
    assert!(matches!(
        parse(vec!["rubric".into()]),
        Ok(Command::Rubric(_))
    ));
    assert!(matches!(
        parse(vec!["gaps".into()]),
        Ok(Command::Gaps { snapshot: None, .. })
    ));
    // A directory named after a subcommand is still assessable through `scan`.
    assert_eq!(scanned(vec!["scan", "show"]), vec![PathBuf::from("show")]);
    assert_eq!(
        scanned(vec!["scan", "src", "--refresh"]),
        vec![PathBuf::from("src")]
    );
    assert!(matches!(
        parse(vec!["show".into()]),
        Ok(Command::Show { snapshot: None, .. })
    ));
    assert!(matches!(
        parse(vec!["snapshots".into()]),
        Ok(Command::Snapshots { .. })
    ));
}

#[test]
fn reading_a_snapshot_takes_a_selector_and_its_own_options() {
    let id = "q_0123456789abcdef";
    match parse(vec![
        "show".into(),
        id.into(),
        "--json".into(),
        "--limit".into(),
        "0".into(),
    ]) {
        Ok(Command::Show {
            snapshot,
            json,
            limit,
        }) => {
            assert_eq!(snapshot.as_deref(), Some(id));
            assert!(json);
            assert_eq!(limit, 0);
        }
        other => panic!("expected a show view, got {:?}", other.is_ok()),
    }
    match parse(vec![
        "dimension".into(),
        "maintainability".into(),
        id.into(),
    ]) {
        Ok(Command::Dimension {
            name,
            snapshot,
            limit,
            ..
        }) => {
            assert_eq!(name, "maintainability");
            assert_eq!(snapshot.as_deref(), Some(id));
            assert_eq!(limit, query::DEFAULT_LIMIT);
        }
        _ => panic!("expected a dimension view"),
    }
    match parse(vec!["file".into(), "src/a.ts".into()]) {
        Ok(Command::File { path, snapshot, .. }) => {
            assert_eq!(path, "src/a.ts");
            assert_eq!(snapshot, None);
        }
        _ => panic!("expected a file view"),
    }
    assert!(parse(vec!["dimension".into()]).is_err());
    assert!(parse(vec!["file".into()]).is_err());
    assert!(
        parse(vec![
            "file".into(),
            "a.ts".into(),
            "--limit".into(),
            "5".into()
        ])
        .is_err()
    );
    assert!(parse(vec!["show".into(), "--limit".into(), "many".into()]).is_err());
    assert!(parse(vec!["show".into(), "--refresh".into()]).is_err());
}

/// A cached answer for one whole-file request, with `adjust` free to weaken
/// individual constructs before it is saved.
fn seed(temp: &Temp, path: &str, source: &str, adjust: impl Fn(&mut ApiResponse)) -> String {
    let request = request(path, source, None);
    let hash = digest(&serde_json::to_vec(&request).unwrap());
    let mut response = response(&request);
    adjust(&mut response);
    validate(&response, &request).unwrap();
    save(
        &store::responses(&temp.0).join(format!("{hash}.json")),
        &CacheEntry {
            request_hash: hash.clone(),
            response,
            elapsed_ms: 12,
        },
    )
    .unwrap();
    hash
}

/// Put one construct on a chosen level, keeping the distribution consistent.
fn at_level(response: &mut ApiResponse, construct: &str, level: usize) {
    if let Answer::Score {
        score,
        probabilities,
        ..
    } = response
        .answers
        .get_mut(&format!("{construct}_score"))
        .unwrap()
    {
        *score = level as f64;
        for (index, p) in probabilities {
            *p = if *index == level.to_string() {
                1.0
            } else {
                0.0
            };
        }
    }
}

#[test]
fn a_scan_saves_a_snapshot_that_reads_back_with_no_key_and_no_network() {
    let temp = Temp::new();
    let (weak, strong) = ("export const weak = 1;", "export const strong = 2;");
    temp.write("src/weak.ts", weak);
    temp.write("src/strong.ts", strong);
    seed(&temp, "src/weak.ts", weak, |response| {
        at_level(response, "maintainability", 1);
        at_level(response, "overall", 1);
    });
    seed(&temp, "src/strong.ts", strong, |_| {});
    let options = Options {
        paths: vec!["src".into()],
        json: true,
        ..Default::default()
    };
    // Two files make a scope wider than a file, which cannot be answered from
    // an empty cache. The files are still assessed and reported.
    let (first, errors) = run(&temp.0, &options, None).unwrap();
    assert!(errors);
    assert!(
        first["aggregate_warning"]
            .as_str()
            .unwrap()
            .contains("the files were assessed but no wider scope was")
    );
    assert_eq!(first["files"].as_array().unwrap().len(), 2);
    aggregate::seed(&temp.0, first["files"].as_array().unwrap(), None, response).unwrap();

    let (report, errors) = run(&temp.0, &options, None).unwrap();
    assert!(!errors);
    assert_eq!(report["saved"], true);
    let id = report["id"].as_str().unwrap().to_owned();
    assert!(store::is_snapshot_id(&id));

    // The snapshot records the grades and points at the answers; it does not
    // copy them, and the scan report still carries them.
    let (manifest, files) = store::read(&temp.0, &id).unwrap();
    assert_eq!(manifest["counts"]["assessed"], 2);
    assert_eq!(manifest["rubric_version"], RUBRIC_VERSION);
    assert_eq!(manifest["paths"][0], "src");
    let recorded = &files["files"][0];
    assert!(recorded.get("raw_response").is_none());
    assert!(recorded["request_hash"].is_string());
    assert!(recorded["declarations"].is_array());
    assert!(report["files"][0]["raw_response"].is_object());

    // Every read view works from the saved snapshot alone.
    let view = query::show(&temp.0, None, 0).unwrap();
    assert_eq!(view["snapshot_id"], id.as_str());
    assert_eq!(view["weakest_first"][0]["path"], "src/weak.ts");
    assert_eq!(view["total_files"], 2);
    let maintainability = view["constructs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|construct| construct["id"] == "maintainability")
        .unwrap();
    assert_eq!(maintainability["marked"], 1);
    assert_eq!(maintainability["weakest"]["path"], "src/weak.ts");

    let ranked = query::dimension(&temp.0, "maintainability", Some(&id), 0).unwrap();
    assert_eq!(ranked["construct"]["review_below"], 6.9);
    assert_eq!(ranked["files"][0]["path"], "src/weak.ts");
    assert_eq!(ranked["files"][0]["review_recommended"], true);
    assert_eq!(ranked["files"][1]["path"], "src/strong.ts");
    assert!(query::dimension(&temp.0, "invented", Some(&id), 0).is_err());

    // The file view explains a grade with the wording Jev actually chose,
    // which only the cached answer carries.
    let file = query::file(&temp.0, "src/weak.ts", Some(&id)).unwrap();
    assert_eq!(file["scopes"][0]["scope"], "file");
    assert_eq!(file["scopes"][0]["response_available"], true);
    let construct = file["scopes"][0]["constructs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|construct| construct["id"] == "maintainability")
        .unwrap();
    assert_eq!(construct["level_index"], "1");
    assert!(
        construct["level"]
            .as_str()
            .unwrap()
            .starts_with("Disagree:")
    );
    assert!(query::render(&file).contains("Jev chose: Disagree:"));
    assert!(query::file(&temp.0, "src/missing.ts", Some(&id)).is_err());

    // Without the cached answer the snapshot still reads; only the wording goes.
    fs::remove_dir_all(store::responses(&temp.0)).unwrap();
    let file = query::file(&temp.0, "src/weak.ts", Some(&id)).unwrap();
    assert_eq!(file["scopes"][0]["response_available"], false);
    assert!(file["scopes"][0]["constructs"][0]["level"].is_null());
    assert_eq!(file["scopes"][0]["constructs"][0]["id"], "maintainability");
    assert!(
        (file["scopes"][0]["constructs"][0]["score"]
            .as_f64()
            .unwrap()
            - 10.0 / 3.0)
            .abs()
            < 1e-9,
        "the grade survives without the answer behind it"
    );
    assert!(query::render(&file).contains("no longer cached"));
}

#[test]
fn snapshots_are_immutable_and_only_a_snapshot_id_opens_one() {
    let temp = Temp::new();
    let files = json!({"files": []});
    let (first, _) = store::identity().unwrap();
    store::write(
        &temp.0,
        &first,
        &json!({"created_at": "2026-09-17T00-00-00-000000Z"}),
        &files,
    )
    .unwrap();
    // An id is never reused, and an existing one is refused rather than revised.
    assert!(
        store::write(&temp.0, &first, &json!({"created_at": "later"}), &files)
            .unwrap_err()
            .contains("already exists")
    );
    let (second, _) = store::identity().unwrap();
    assert_ne!(first, second);
    store::write(
        &temp.0,
        &second,
        &json!({"created_at": "2026-09-17T01-00-00-000000Z"}),
        &files,
    )
    .unwrap();

    assert_eq!(store::resolve(&temp.0, None).unwrap(), second);
    assert_eq!(store::resolve(&temp.0, Some("latest")).unwrap(), second);
    assert_eq!(store::resolve(&temp.0, Some(&first)).unwrap(), first);
    let listed = store::list(&temp.0).unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].0, second, "most recent first");

    // A selector is checked before any path is built from it.
    for selector in [
        "..",
        "../../etc/passwd",
        "q_0123",
        "run_0123456789abcdef",
        "",
    ] {
        assert!(store::resolve(&temp.0, Some(selector)).is_err());
        assert!(store::read(&temp.0, selector).is_err());
    }
    // A pointer at a snapshot that is gone falls back to the newest kept one.
    fs::remove_dir_all(store::root(&temp.0).join("snapshots").join(&second)).unwrap();
    assert_eq!(store::resolve(&temp.0, None).unwrap(), first);
    // And with nothing left, browsing says so instead of inventing a snapshot.
    fs::remove_dir_all(store::root(&temp.0).join("snapshots")).unwrap();
    assert!(
        store::resolve(&temp.0, None)
            .unwrap_err()
            .contains("no quality snapshot here yet")
    );
}

#[test]
fn a_windowed_file_is_navigated_by_its_weakest_window() {
    let temp = Temp::new();
    let source = oversized_typescript();
    temp.write("big.ts", &source);
    let starts = line_starts(&source);
    let (outline, planned) = plan("big.ts", &source, None).unwrap();
    // The second window is the weak one, so navigation must lead there.
    for (position, (window, _)) in planned.iter().enumerate() {
        let text = window_source(&source, &starts, window);
        let request = windowed_request("big.ts", text, window, &outline, None);
        let hash = digest(&serde_json::to_vec(&request).unwrap());
        let mut answers = response(&request);
        if position == 1 {
            at_level(&mut answers, "maintainability", 0);
        }
        validate(&answers, &request).unwrap();
        save(
            &store::responses(&temp.0).join(format!("{hash}.json")),
            &CacheEntry {
                request_hash: hash,
                response: answers,
                elapsed_ms: 3,
            },
        )
        .unwrap();
    }
    let options = Options {
        paths: vec!["big.ts".into()],
        json: true,
        ..Default::default()
    };
    let (report, errors) = run(&temp.0, &options, None).unwrap();
    assert!(!errors);
    let id = report["id"].as_str().unwrap().to_owned();
    assert_eq!(store::read(&temp.0, &id).unwrap().0["counts"]["partial"], 1);

    let weak = &planned[1].0;
    let ranked = query::dimension(&temp.0, "maintainability", Some(&id), 0).unwrap();
    assert_eq!(ranked["files"][0]["path"], "big.ts");
    assert_eq!(ranked["files"][0]["score"], 0.0);
    assert_eq!(
        ranked["files"][0]["scope"],
        format!("window 2/2, lines {}-{}", weak.start_line, weak.end_line)
    );
    assert!(query::render(&ranked).contains("window 2/2"));

    let file = query::file(&temp.0, "big.ts", Some(&id)).unwrap();
    assert_eq!(file["partial"], true);
    assert_eq!(file["scopes"].as_array().unwrap().len(), 2);
    assert_eq!(file["scopes"][1]["scope"], "window 2/2");
    assert_eq!(file["scopes"][1]["start_line"], weak.start_line);
    let text = query::render(&file);
    assert!(text.contains("no whole-file grade"));
    assert!(text.contains("Top-level declarations, none assessed on its own"));
    assert!(text.contains("step0 (function)"));
}

#[test]
fn a_wider_scope_reads_verdicts_and_never_a_score_or_a_line_of_source() {
    let temp = Temp::new();
    let sources = [
        (
            "src/lib/alpha.ts",
            "export function alpha() {\n  return \"distinctiveSourceMarker\";\n}\n",
        ),
        (
            "src/lib/beta.ts",
            "export function beta() {\n  return 2;\n}\n",
        ),
        (
            "src/gamma.ts",
            "export function gamma() {\n  return 3;\n}\n",
        ),
    ];
    for (path, source) in sources {
        temp.write(path, source);
        seed(&temp, path, source, |_| {});
    }
    let options = Options {
        paths: vec!["src".into()],
        json: true,
        ..Default::default()
    };
    let (first, _) = run(&temp.0, &options, None).unwrap();
    let sent = std::cell::RefCell::new(Vec::new());
    aggregate::seed(
        &temp.0,
        first["files"].as_array().unwrap(),
        None,
        |request| {
            sent.borrow_mut().push(request.clone());
            response(request)
        },
    )
    .unwrap();

    let sent = sent.into_inner();
    assert!(!sent.is_empty(), "a wider scope was judged");
    for request in &sent {
        let state = request["state"].to_string();
        // What Jev reads is the level it chose for each part, word for word.
        assert!(state.contains("Strongly agree"), "verdicts are quoted");
        assert!(state.contains("assessment_basis"));
        // What it must never read: the source, a score, or a review policy.
        assert!(!state.contains("distinctiveSourceMarker"), "no source");
        assert!(!state.contains("\"score\""), "no score");
        assert!(!state.contains("review_below"), "no cutoff");
        assert!(!state.contains("/10"), "no scale");
        // Every construct asked here is a Score or a Noul with its own wording.
        assert!(request["questions"]["basis"]["type"] == "noul");
        assert!(request["questions"]["overall_score"]["criteria"].is_array());
    }

    let (report, errors) = run(&temp.0, &options, None).unwrap();
    assert!(!errors);
    // src/lib holds two files, so it is judged, and the repository reads its
    // verdict rather than the files beneath it.
    let scopes: Vec<(&str, &str)> = report["aggregates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["scope"].as_str().unwrap(),
                entry["path"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        scopes,
        vec![("directory", "src/lib"), ("repository", "src")],
        "deepest scope first, the repository last"
    );
    let repository = &report["repository"];
    assert_eq!(repository["path"], "src");
    assert!(repository["dimensions"]["maintainability"]["score"].is_number());
    // Nothing at this scope is calibrated, so nothing here is marked.
    assert!(repository["dimensions"]["maintainability"]["review_below"].is_null());
    assert!(repository["dimensions"]["maintainability"]["review_recommended"].is_null());

    let view = query::show(&temp.0, None, 0).unwrap();
    assert_eq!(view["repository"]["path"], "src");
    assert_eq!(view["directories"].as_array().unwrap().len(), 1);
    let text = query::render(&view);
    assert!(text.contains("Repository — Jev's own judgment"));
    assert!(text.contains("Directories, each judged by Jev"));
    assert!(text.contains("src/lib"));
}

#[test]
fn a_repository_judgment_is_withheld_when_jev_says_the_evidence_is_too_thin() {
    let mut repository = json!({
        "scope": "repository", "path": ".", "basis": 0.18, "basis_sufficient": false,
        "dimensions": {
            "maintainability": {"score": 6.0, "confidence": 0.8},
            "readability": {"score": 6.0, "confidence": 0.8},
            "overall": {"score": 6.0, "confidence": 0.8},
        },
    });
    let view = json!({
        "view": "show", "snapshot_id": "q_0123456789abcdef",
        "manifest": {"counts": {}, "usage_this_run": {}},
        "repository": repository, "directories": [], "constructs": [],
        "total_files": 0, "shown": 0, "weakest_first": [], "errors": [],
    });
    let text = query::render(&view);
    assert!(text.contains("no judgment shown"));
    assert!(text.contains("Jev answered 0.18 of 1"));
    assert!(
        !text.contains("6.00/10"),
        "a withheld grade is not shown anyway"
    );

    // The same grades are presented once Jev says the evidence carries them.
    repository["basis"] = json!(0.74);
    repository["basis_sufficient"] = json!(true);
    let mut view = view;
    view["repository"] = repository;
    let text = query::render(&view);
    assert!(text.contains("Repository — Jev's own judgment"));
    assert!(text.contains("6.00/10"));
    assert!(text.contains("basis 0.74/1"));
}

const NESTED: &str = "export function alpha(input: string): string {\n  const helper = () => input.trim();\n  return helper();\n}\n\nexport class Beta {\n  run(): number {\n    return 1;\n  }\n}\n";

/// Answer and cache every declaration request for one file, so a deepen can
/// then run offline. This drives the real request builder.
fn seed_declarations(temp: &Temp, path: &str, source: &str, adjust: impl Fn(&mut ApiResponse)) {
    let mut answer = |request: &Value,
                      bytes: &[u8]|
     -> Result<(String, CacheEntry, bool, Option<String>), String> {
        let hash = digest(bytes);
        let mut response = response(request);
        adjust(&mut response);
        validate(&response, request).unwrap();
        let entry = CacheEntry {
            request_hash: hash.clone(),
            response,
            elapsed_ms: 4,
        };
        save(
            &store::responses(&temp.0).join(format!("{hash}.json")),
            &entry,
        )
        .unwrap();
        Ok((hash, entry, false, None))
    };
    declarations::assess(path, source, None, &mut answer).unwrap();
}

#[test]
fn gradable_declarations_are_functions_and_methods_but_never_nested_closures() {
    let found = declarations::gradable("src/a.ts", NESTED).unwrap();
    let named: Vec<(&str, &str)> = found
        .iter()
        .map(|declaration| (declaration.name.as_str(), declaration.kind.as_str()))
        .collect();
    assert_eq!(named, vec![("alpha", "function"), ("Beta.run", "method")]);
    // A callback installed at the top level is the code that installs it.
    let handlers = declarations::gradable(
        "src/b.js",
        "process.on(\"exit\", () => {\n  cleanup();\n});\nfunction cleanup() {}\n",
    )
    .unwrap();
    assert!(handlers.iter().any(|found| found.name == "cleanup"));
    // A language Supercov cannot parse has no declarations to offer, which is
    // not the same as a parsed file that declares none.
    assert!(declarations::gradable("a.cs", "class A { void B() {} }").is_none());
}

#[test]
fn deepening_grades_declarations_and_leaves_its_parent_snapshot_untouched() {
    let temp = Temp::new();
    temp.write("src/a.ts", NESTED);
    seed(&temp, "src/a.ts", NESTED, |_| {});
    let options = Options {
        paths: vec!["src".into()],
        json: true,
        ..Default::default()
    };
    let (report, errors) = run(&temp.0, &options, None).unwrap();
    assert!(!errors);
    let parent = report["id"].as_str().unwrap().to_owned();
    let declared: Vec<&str> = report["files"][0]["declarations"]
        .as_array()
        .unwrap()
        .iter()
        .map(|declaration| declaration["name"].as_str().unwrap())
        .collect();
    assert_eq!(declared, vec!["alpha", "Beta.run"]);

    // Before deepening, a declaration says it has no grade and how to get one.
    let listed = query::functions(&temp.0, "src/a.ts", None, 0).unwrap();
    assert_eq!(listed["total"], 2);
    assert_eq!(listed["assessed"], 0);
    assert_eq!(listed["declarations"][0]["assessed"], false);
    let text = query::render(&listed);
    assert!(text.contains("not assessed"));
    assert!(text.contains("supercov quality functions src/a.ts --deepen"));
    let one = query::function(&temp.0, "src/a.ts", "alpha", None, false).unwrap();
    assert_eq!(one["assessed"], false);
    assert!(query::render(&one).contains("Not assessed on its own"));

    // Deepening asks about every declaration, and the weaker one leads.
    seed_declarations(&temp, "src/a.ts", NESTED, |response| {
        at_level(response, "d0_maintainability", 0);
    });
    let deepened = deepen(&temp.0, "src/a.ts", None, None, false, None).unwrap();
    let child = deepened["id"].as_str().unwrap().to_owned();
    assert_ne!(child, parent);
    assert_eq!(deepened["parent"], parent.as_str());
    assert_eq!(deepened["deepened"][0], "src/a.ts");
    assert_eq!(deepened["counts"]["declarations"], 2);
    assert_eq!(deepened["declarations"].as_array().unwrap().len(), 2);

    // The parent still holds exactly what it held.
    let (parent_manifest, parent_files) = store::read(&temp.0, &parent).unwrap();
    assert!(parent_manifest["parent"].is_null());
    assert!(parent_files["files"][0]["assessed_declarations"].is_null());

    let listed = query::functions(&temp.0, "src/a.ts", Some(&child), 0).unwrap();
    assert_eq!(listed["assessed"], 2);
    assert_eq!(listed["parent"], parent.as_str());
    assert_eq!(listed["declarations"][0]["name"], "alpha", "weakest first");
    assert_eq!(
        listed["declarations"][0]["dimensions"]["maintainability"]["score"],
        0.0
    );
    assert!(query::render(&listed).contains("Deepened from snapshot"));

    let one = query::function(&temp.0, "src/a.ts", "Beta.run", Some(&child), false).unwrap();
    assert_eq!(one["assessed"], true);
    assert_eq!(one["graded_within"], "the whole file");
    assert_eq!(one["substance"], 0.99);
    let readability = one["constructs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|construct| construct["id"] == "readability")
        .unwrap();
    assert!(
        readability["level"]
            .as_str()
            .unwrap()
            .starts_with("Strongly agree:"),
        "the wording Jev chose, read back from the cached answer"
    );
    assert!(query::render(&one).contains("Jev chose: Strongly agree:"));

    // The graded source can be shown because the file still holds those bytes.
    let with_source = query::function(&temp.0, "src/a.ts", "alpha", Some(&child), true).unwrap();
    let source = with_source["source"].as_str().unwrap();
    assert!(source.contains("export function alpha"));
    assert!(!source.contains("class Beta"), "only its own lines");
}

#[test]
fn deepening_refuses_source_that_is_no_longer_what_was_graded() {
    let temp = Temp::new();
    temp.write("src/a.ts", NESTED);
    seed(&temp, "src/a.ts", NESTED, |_| {});
    let options = Options {
        paths: vec!["src".into()],
        json: true,
        ..Default::default()
    };
    let (report, _) = run(&temp.0, &options, None).unwrap();
    let snapshot = report["id"].as_str().unwrap().to_owned();
    seed_declarations(&temp, "src/a.ts", NESTED, |_| {});
    let child = deepen(&temp.0, "src/a.ts", None, None, false, None).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();

    // A context document the snapshot was not assessed with is refused, while
    // the source is still the graded source.
    temp.write("contract.md", "a contract");
    assert!(
        deepen(
            &temp.0,
            "src/a.ts",
            Some(&snapshot),
            Some(Path::new("contract.md")),
            false,
            None
        )
        .unwrap_err()
        .contains("different --context document")
    );

    temp.write(
        "src/a.ts",
        &NESTED.replace("input.trim()", "input.trimEnd()"),
    );
    let refusal = deepen(&temp.0, "src/a.ts", Some(&snapshot), None, false, None).unwrap_err();
    assert!(refusal.contains("has changed since snapshot"));
    // The grades stay readable; only the source view is withheld.
    assert_eq!(
        query::function(&temp.0, "src/a.ts", "alpha", Some(&child), false).unwrap()["assessed"],
        true
    );
    let refusal = query::function(&temp.0, "src/a.ts", "alpha", Some(&child), true).unwrap_err();
    assert!(refusal.contains("its graded source is gone"));

    assert!(
        deepen(&temp.0, "src/missing.ts", None, None, false, None)
            .unwrap_err()
            .contains("is not in snapshot")
    );
}

#[test]
fn a_declaration_request_names_every_declaration_it_asks_about() {
    let declarations = declarations::gradable("src/a.ts", NESTED).unwrap();
    let mut sent = Vec::new();
    let mut answer = |request: &Value,
                      bytes: &[u8]|
     -> Result<(String, CacheEntry, bool, Option<String>), String> {
        sent.push(request.clone());
        Ok((
            digest(bytes),
            CacheEntry {
                request_hash: digest(bytes),
                response: response(request),
                elapsed_ms: 1,
            },
            false,
            None,
        ))
    };
    declarations::assess("src/a.ts", NESTED, None, &mut answer).unwrap();
    assert_eq!(sent.len(), 1, "two declarations fit one request");
    let request = &sent[0];
    // The file is supplied once, whole, as shared context.
    assert_eq!(request["state"]["file"]["source"], NESTED);
    assert_eq!(request["state"]["file"]["supplied"], "the whole file");
    // Question ids are routing keys, so each question names its own subject.
    for (index, declaration) in declarations.iter().enumerate() {
        for construct in declarations::order() {
            let instructions =
                &request["questions"][format!("d{index}_{construct}_score")]["instructions"];
            assert_eq!(instructions["declaration"], declaration.name);
            assert_eq!(instructions["kind"], declaration.kind);
            assert_eq!(
                instructions["lines"],
                format!("{}-{}", declaration.start_line, declaration.end_line)
            );
            assert_eq!(instructions["axis"], construct);
        }
        assert_eq!(
            request["questions"][format!("d{index}_substance")]["type"],
            "noul"
        );
    }
}

#[test]
fn a_diff_shows_what_changed_and_marks_movements_repeat_variation_explains() {
    let temp = Temp::new();
    let before = "export function alpha() {\n  return 1;\n}\n";
    temp.write("src/a.ts", before);
    seed(&temp, "src/a.ts", before, |response| {
        at_level(response, "maintainability", 1);
        at_level(response, "readability", 1);
    });
    let options = Options {
        paths: vec!["src".into()],
        json: true,
        ..Default::default()
    };
    // One file has no wider scope, so this scan completes from the cache alone.
    let (first, errors) = run(&temp.0, &options, None).unwrap();
    assert!(!errors);
    let from = first["id"].as_str().unwrap().to_owned();

    let after =
        "export function alpha() {\n  return 1;\n}\n\nexport function gamma() {\n  return 3;\n}\n";
    temp.write("src/a.ts", after);
    seed(&temp, "src/a.ts", after, |response| {
        at_level(response, "maintainability", 2);
        at_level(response, "readability", 1);
    });
    let (second, errors) = run(&temp.0, &options, None).unwrap();
    assert!(!errors);
    let to = second["id"].as_str().unwrap().to_owned();

    let view = query::diff(&temp.0, &from, &to, 0).unwrap();
    assert_eq!(view["files"]["in_both"], 1);
    assert_eq!(view["files"]["changed_source"], 1);
    assert_eq!(view["files"]["unchanged"], 0);
    assert!(view["files"]["regraded"].as_array().unwrap().is_empty());
    let changed = &view["files"]["changed"][0];
    assert_eq!(changed["path"], "src/a.ts");
    // Maintainability moved a level; readability stayed and is left out.
    let moved: Vec<&str> = changed["constructs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|construct| construct["id"].as_str().unwrap())
        .collect();
    assert_eq!(moved, vec!["maintainability"]);
    let maintainability = &changed["constructs"][0];
    assert!((maintainability["from"].as_f64().unwrap() - 10.0 / 3.0).abs() < 1e-9);
    assert!((maintainability["to"].as_f64().unwrap() - 20.0 / 3.0).abs() < 1e-9);
    assert_eq!(maintainability["within_repeat_variation"], false);
    assert_eq!(changed["declarations"]["added"], json!(["gamma"]));
    assert!(
        changed["declarations"]["removed"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let text = query::render(&view);
    assert!(text.contains("1 files in both"));
    assert!(text.contains("maintainability 3.33 → 6.67 (+3.33)"));
    assert!(text.contains("declarations added: gamma"));
    assert!(!text.contains("within measured repeat variation"));

    // Comparing a snapshot with itself is a mistake worth naming.
    assert!(
        query::diff(&temp.0, &from, &from, 0)
            .unwrap_err()
            .contains("the same snapshot twice")
    );
}

#[test]
fn a_repeat_of_one_question_is_reported_apart_from_a_change_in_the_code() {
    let temp = Temp::new();
    let file = |score: f64| {
        json!({
            "path": "src/a.ts", "status": "completed", "source_hash": "the-same-bytes",
            "declarations": [], "dimensions": {
                "maintainability": {"score": score, "confidence": 0.5},
            },
        })
    };
    let manifest = |created: &str| {
        json!({
            "created_at": created, "rubric_version": RUBRIC_VERSION,
            "policy_version": POLICY_VERSION, "model": MODEL,
        })
    };
    let (from, _) = store::identity().unwrap();
    store::write(
        &temp.0,
        &from,
        &manifest("2026-09-17T00-00-00-000000Z"),
        &json!({"files": [file(6.0)]}),
    )
    .unwrap();
    let (to, _) = store::identity().unwrap();
    store::write(
        &temp.0,
        &to,
        &manifest("2026-09-17T01-00-00-000000Z"),
        &json!({"files": [file(6.5)], "aggregates": []}),
    )
    .unwrap();

    let view = query::diff(&temp.0, &from, &to, 0).unwrap();
    // The source did not change, so this is the same question asked twice.
    assert_eq!(view["files"]["changed_source"], 0);
    let regraded = view["files"]["regraded"].as_array().unwrap();
    assert_eq!(regraded.len(), 1);
    let moved = &regraded[0]["constructs"][0];
    assert_eq!(moved["delta"], 0.5);
    assert_eq!(
        moved["within_repeat_variation"], true,
        "half a point is inside the variation two identical requests have shown"
    );
    let text = query::render(&view);
    assert!(text.contains("not changes in the code"));
    assert!(text.contains("within measured repeat variation"));

    // A different rubric asks different questions, so the grades do not compare.
    let (other, _) = store::identity().unwrap();
    let mut different = manifest("2026-09-17T02-00-00-000000Z");
    different["rubric_version"] = json!("quality-v2-experimental");
    store::write(&temp.0, &other, &different, &json!({"files": []})).unwrap();
    let refusal = query::diff(&temp.0, &from, &other, 0).unwrap_err();
    assert!(refusal.contains("only snapshots of the same rubric and model can be compared"));
}

/// One class holding everything, the shape most Java files have, and far past
/// the budget.
fn oversized_java_class() -> String {
    let mut source = String::from("package example;\n\npublic class Huge {\n");
    for index in 0..60 {
        source.push_str(&format!(
            "  public int step{index}(int input) {{\n{}    return input + {index};\n  }}\n\n",
            "    // a line of body that exists only to take up room in this method\n".repeat(30),
        ));
    }
    source.push_str("}\n");
    source
}

#[test]
fn a_file_that_is_one_large_class_is_split_at_that_class_s_methods() {
    let source = oversized_java_class();
    assert!(
        within_budget(&request("Huge.java", &source, None))
            .unwrap()
            .is_none(),
        "the fixture has to be over budget for this to mean anything"
    );
    let starts = line_starts(&source);
    let (outline, planned) = plan("Huge.java", &source, None).unwrap();
    // The class is the only top-level declaration, so splitting at top-level
    // boundaries alone would leave one window and no way to send the file.
    assert!(planned.len() >= 2, "a single class still splits");
    assert!(planned.iter().all(|(_, oversized)| !oversized));

    let named: Vec<&str> = outline
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| unit["name"].as_str().unwrap())
        .collect();
    assert!(
        named.len() >= 60,
        "split points are the methods: {}",
        named.len()
    );
    assert!(
        named.iter().all(|name| name.starts_with("Huge.")),
        "every split point is inside the class: {named:?}"
    );

    // Still a partition: every line in exactly one window, nothing duplicated.
    let mut rebuilt = String::new();
    for (window, _) in &planned {
        rebuilt.push_str(window_source(&source, &starts, window));
        assert!(
            within_budget(&windowed_request("Huge.java", "x", window, &outline, None))
                .unwrap()
                .is_some()
        );
    }
    assert_eq!(rebuilt, source);
    assert_eq!(
        planned[0].0.start_line, 1,
        "the class header joins the first window"
    );
    assert_eq!(planned.last().unwrap().0.end_line, starts.len());
}

#[test]
fn a_declaration_small_enough_to_send_stays_one_split_point() {
    // Descending is for declarations too large to send, not for every class:
    // a small class keeps its own boundary rather than exposing its methods.
    let source = format!(
        "package example;\n\npublic class Small {{\n  public int a() {{ return 1; }}\n  public int b() {{ return 2; }}\n}}\n\npublic class Other {{\n{}  public int c() {{ return 3; }}\n}}\n",
        "  // filler\n".repeat(4),
    );
    let (outline, _) = split_units("Small.java", &source).unwrap();
    let named: Vec<&str> = outline
        .as_array()
        .unwrap()
        .iter()
        .map(|unit| unit["name"].as_str().unwrap())
        .collect();
    assert_eq!(named, vec!["Small", "Other"]);
}

#[test]
fn every_request_gets_its_own_answer_back_when_they_are_sent_together() {
    let temp = Temp::new();
    // Twenty cached answers, so the whole batch resolves without a network or a
    // key, and each one is distinguishable by its own source.
    let mut pending = Vec::new();
    for index in 0..20 {
        let source = format!("export const value{index} = {index};");
        let path = format!("src/file{index}.ts");
        let request = request(&path, &source, None);
        let bytes = serde_json::to_vec(&request).unwrap();
        let hash = digest(&bytes);
        let mut response = response(&request);
        // Each answer carries a different overall level, so a mixed-up slot
        // would show as the wrong grade rather than passing unnoticed.
        at_level(&mut response, "overall", index % 5);
        validate(&response, &request).unwrap();
        save(
            &store::responses(&temp.0).join(format!("{hash}.json")),
            &CacheEntry {
                request_hash: hash,
                response,
                elapsed_ms: 1,
            },
        )
        .unwrap();
        pending.push(((index, None), request, bytes));
    }
    let answered = answer_all(&temp.0, &client(), None, false, &pending, false);
    assert_eq!(answered.len(), pending.len(), "one answer per request");
    for (index, _) in pending.iter().enumerate().take(20) {
        let (_, entry, hit, _) = answered[&(index, None)].as_ref().unwrap();
        assert!(hit, "every answer came from the cache");
        let Answer::Score { score, .. } = entry.response.answers["overall_score"] else {
            unreachable!("a validated score")
        };
        assert_eq!(
            score,
            (index % 5) as f64,
            "answer {index} landed in its own slot"
        );
    }
    // A request with no cached answer and no key fails on its own, without
    // taking its neighbours down.
    let missing = request("src/absent.ts", "export const absent = 1;", None);
    let bytes = serde_json::to_vec(&missing).unwrap();
    pending.push(((99, None), missing, bytes));
    let answered = answer_all(&temp.0, &client(), None, false, &pending, false);
    assert!(answered[&(99, None)].is_err());
    assert_eq!(
        answered.values().filter(|answer| answer.is_ok()).count(),
        20
    );
}

// ---- named-property catalog -------------------------------------------------

fn values(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
    pairs.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()
}

#[test]
fn catalog_parses_and_holds_only_checks_the_evidence_kept() {
    let catalog = smells::catalog();
    assert_eq!(catalog.len(), 12);
    let ids: Vec<&str> = catalog.iter().map(|c| c.id.as_str()).collect();
    // Cut for firing on everything rather than for being wrong, and for never
    // firing at all. Re-adding one silently would undo a measured decision.
    for gone in ["mixed_abstraction", "comment_deodorant", "data_class"] {
        assert!(!ids.contains(&gone), "{gone} was cut from the catalog");
    }
    for check in catalog {
        assert!(!check.task.is_empty() && !check.present.is_empty());
        assert!(!check.absent.is_empty() && !check.evidence.is_empty());
    }
}

#[test]
fn health_is_ten_when_nothing_fires_and_zero_when_everything_does() {
    assert_eq!(
        smells::health(&values(&[("a", 0.0), ("b", 0.0)])),
        Some(10.0)
    );
    assert_eq!(
        smells::health(&values(&[("a", 1.0), ("b", 1.0)])),
        Some(0.0)
    );
    assert_eq!(
        smells::health(&values(&[("a", 0.0), ("b", 1.0)])),
        Some(5.0)
    );
    assert_eq!(smells::health(&BTreeMap::new()), None);
}

#[test]
fn health_uses_the_mean_so_the_scale_survives_a_catalog_change() {
    // Two checks at 0.5 and four checks at 0.5 are the same health. A sum would
    // make the number mean something different after a check is added.
    let two = smells::health(&values(&[("a", 0.5), ("b", 0.5)]));
    let four = smells::health(&values(&[("a", 0.5), ("b", 0.5), ("c", 0.5), ("d", 0.5)]));
    assert_eq!(two, four);
}

#[test]
fn present_reports_only_what_fired_strongest_first() {
    let fired = smells::present(&values(&[
        ("low", 0.49),
        ("high", 0.91),
        ("edge", 0.5),
        ("mid", 0.7),
    ]));
    let names: Vec<&str> = fired.iter().map(|(id, _)| id.as_str()).collect();
    assert_eq!(names, ["high", "mid", "edge"]);
}

#[test]
fn aggregate_weights_by_size_so_small_files_cannot_outvote_the_code() {
    // Nine tiny perfect files and one large bad one: a plain average would call
    // this healthy, which is the failure weighting exists to prevent.
    let mut files = vec![(10u64, 10.0f64); 9];
    files.push((10_000, 0.0));
    let weighted = smells::aggregate(&files).unwrap();
    assert!(weighted < 0.1, "byte-weighted health was {weighted}");
    assert_eq!(smells::aggregate(&[]), None);
}

#[test]
fn a_change_request_sends_exactly_the_two_versions() {
    // The wrapped shape measurably weakened detection. If this test fails the
    // request is no longer the one the evidence was gathered on.
    let request = smells::change_request("old", "new");
    let state = request["state"].as_object().unwrap();
    assert_eq!(state.len(), 2);
    assert_eq!(state["before"], "old");
    assert_eq!(state["after"], "new");
    assert_eq!(request["questions"].as_object().unwrap().len(), 12);
}

#[test]
fn change_questions_ask_what_appeared_and_file_questions_do_not() {
    let change = smells::change_questions();
    let file = smells::file_questions();
    let task = |q: &Value| q["instructions"]["task"].as_str().unwrap().to_owned();
    assert!(task(&change["deep_nesting"]).contains("does `after` show the following"));
    assert!(!task(&file["deep_nesting"]).contains("`after`"));
    // The underlying question is the same one in both forms.
    assert!(task(&change["deep_nesting"]).ends_with(&task(&file["deep_nesting"])));
}

#[test]
fn a_file_request_names_the_path_but_a_change_request_does_not() {
    let file = smells::file_request("src/a.rs", "fn a() {}");
    assert_eq!(file["state"]["file"]["path"], "src/a.rs");
    assert_eq!(file["state"]["file"]["source"], "fn a() {}");
}

// ---- change ranges ----------------------------------------------------------

#[test]
fn patch_defaults_to_unstaged_and_accepts_each_range() {
    let parse_range = |args: &[&str]| match parse(
        std::iter::once("patch")
            .chain(args.iter().copied())
            .map(str::to_owned)
            .collect(),
    ) {
        Ok(Command::Patch { range, .. }) => Ok(range),
        Ok(_) => panic!("patch did not parse as a patch"),
        Err(e) => Err(e),
    };
    assert_eq!(parse_range(&[]).unwrap(), changes::Range::Unstaged);
    assert_eq!(parse_range(&["--staged"]).unwrap(), changes::Range::Staged);
    assert_eq!(parse_range(&["--cached"]).unwrap(), changes::Range::Staged);
    assert_eq!(
        parse_range(&["--base", "main"]).unwrap(),
        changes::Range::Base("main".into())
    );
    assert!(parse_range(&["--base"]).is_err());
    assert!(parse_range(&["--staged", "--base", "main"]).is_err());
    // The old spelling is gone rather than quietly aliased, because it meant
    // the ref's tip where --base means the merge base.
    assert!(parse_range(&["--since", "main"]).is_err());
    // Naming the same range twice is not a conflict.
    assert!(parse_range(&["--staged", "--cached"]).is_ok());
    assert!(parse_range(&["--nonsense"]).is_err());
}

#[test]
fn patch_takes_paths_and_a_limit() {
    match parse(
        ["patch", "--limit", "3", "src", "docs/a.md"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
    ) {
        Ok(Command::Patch { paths, limit, .. }) => {
            assert_eq!(limit, 3);
            assert_eq!(
                paths,
                vec![PathBuf::from("src"), PathBuf::from("docs/a.md")]
            );
        }
        other => panic!("unexpected parse: {}", other.is_err()),
    }
}

#[test]
fn range_names_itself_the_same_way_every_run() {
    assert_eq!(changes::Range::Unstaged.id(), "unstaged");
    assert_eq!(changes::Range::Staged.id(), "staged");
    assert_eq!(changes::Range::Base("v1".into()).id(), "base:v1");
    assert!(changes::Range::Base("v1".into()).describe().contains("v1"));
}

fn git_in(root: &Path, arguments: &[&str]) {
    let status = std::process::Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .expect("git runs in tests");
    assert!(
        status.status.success(),
        "git {:?}: {}",
        arguments,
        String::from_utf8_lossy(&status.stderr)
    );
}

fn repository() -> Temp {
    let temp = Temp::new();
    git_in(&temp.0, &["init", "--quiet"]);
    git_in(&temp.0, &["config", "user.email", "t@example.invalid"]);
    git_in(&temp.0, &["config", "user.name", "Test"]);
    git_in(&temp.0, &["config", "commit.gpgsign", "false"]);
    temp
}

#[test]
fn unstaged_compares_the_working_tree_with_the_index() {
    let temp = repository();
    temp.write("a.rs", "fn a() {}\n");
    git_in(&temp.0, &["add", "a.rs"]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "first"]);
    temp.write("a.rs", "fn a() { let x = 1; }\n");

    let found = changes::collect(&temp.0, &changes::Range::Unstaged, &[]).unwrap();
    assert_eq!(found.len(), 1);
    // before comes from the index, which is the committed text here. An earlier
    // version built the revision spec as `::a.rs`, git failed, and the error was
    // swallowed into an empty before: every property looked introduced.
    assert_eq!(found[0].before, "fn a() {}\n");
    assert_eq!(found[0].after, "fn a() { let x = 1; }\n");
    assert!(found[0].reviewable());
    assert!(found[0].patch.contains("let x = 1"));
}

#[test]
fn staged_compares_the_index_with_head() {
    let temp = repository();
    temp.write("a.rs", "fn a() {}\n");
    git_in(&temp.0, &["add", "a.rs"]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "first"]);
    temp.write("a.rs", "fn a() { staged(); }\n");
    git_in(&temp.0, &["add", "a.rs"]);
    // Working tree moves on again; --staged must not see this.
    temp.write("a.rs", "fn a() { later(); }\n");

    let found = changes::collect(&temp.0, &changes::Range::Staged, &[]).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].before, "fn a() {}\n");
    assert_eq!(found[0].after, "fn a() { staged(); }\n");
}

#[test]
fn base_compares_the_working_tree_with_the_merge_base_not_the_tip() {
    let temp = repository();
    temp.write("a.rs", "fn a() {}\n");
    git_in(&temp.0, &["add", "a.rs"]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "first"]);
    git_in(&temp.0, &["tag", "base"]);
    temp.write("a.rs", "fn a() { two(); }\n");
    git_in(&temp.0, &["add", "a.rs"]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "second"]);
    temp.write("a.rs", "fn a() { three(); }\n");

    let found = changes::collect(&temp.0, &changes::Range::Base("base".into()), &[]).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].before, "fn a() {}\n");
    assert_eq!(found[0].after, "fn a() { three(); }\n");
    assert!(changes::collect(&temp.0, &changes::Range::Base("nope".into()), &[]).is_err());
}

#[test]
fn a_base_that_moved_on_is_not_this_branch_s_obligation() {
    // The whole reason for the merge base. main gains a commit after the branch
    // leaves it; comparing against main's tip would report that commit's file as
    // part of this change.
    let temp = repository();
    temp.write("shared.rs", "fn shared() {}\n");
    git_in(&temp.0, &["add", "."]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "first"]);
    git_in(&temp.0, &["checkout", "--quiet", "-b", "work"]);
    temp.write("mine.rs", "fn mine() {}\n");
    git_in(&temp.0, &["add", "."]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "mine"]);

    let main = String::from_utf8(
        std::process::Command::new("git")
            .current_dir(&temp.0)
            .args(["rev-parse", "HEAD~1"])
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    git_in(&temp.0, &["checkout", "--quiet", main.trim()]);
    git_in(&temp.0, &["checkout", "--quiet", "-B", "main"]);
    temp.write("theirs.rs", "fn theirs() {}\n");
    git_in(&temp.0, &["add", "."]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "theirs"]);
    git_in(&temp.0, &["checkout", "--quiet", "work"]);

    let found = changes::collect(&temp.0, &changes::Range::Base("main".into()), &[]).unwrap();
    let paths: Vec<&str> = found.iter().map(|c| c.path.as_str()).collect();
    assert!(paths.contains(&"mine.rs"), "{paths:?}");
    assert!(
        !paths.contains(&"theirs.rs"),
        "a commit landed on main after this branch left it is not this change: {paths:?}"
    );
}

#[test]
fn an_untracked_file_is_reviewed_as_an_addition() {
    let temp = repository();
    temp.write("tracked.rs", "fn tracked() {}\n");
    git_in(&temp.0, &["add", "."]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "first"]);
    temp.write("brand-new.rs", "fn fresh() {}\n");

    // git diff never mentions an untracked file, but the author wrote it.
    let found = changes::collect(&temp.0, &changes::Range::Unstaged, &[]).unwrap();
    let new = found.iter().find(|c| c.path == "brand-new.rs").unwrap();
    assert_eq!(new.kind, changes::Kind::Added);
    assert_eq!(new.after, "fn fresh() {}\n");
    assert!(new.before.is_empty());
    // Staged work is the exception: an untracked file is not in the index.
    let staged = changes::collect(&temp.0, &changes::Range::Staged, &[]).unwrap();
    assert!(staged.iter().all(|c| c.path != "brand-new.rs"));
}

#[test]
fn an_annotation_anchors_at_the_first_line_the_change_adds() {
    let patch = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -10,3 +12,7 @@ fn a() {\n+    added\n";
    assert_eq!(changes::first_added_line(patch), Some(12));
    assert_eq!(changes::first_added_line("no hunks here"), None);
}

#[test]
fn an_addition_has_no_previous_version_and_a_deletion_is_not_reviewed() {
    let temp = repository();
    temp.write("keep.rs", "fn keep() {}\n");
    temp.write("gone.rs", "fn gone() {}\n");
    git_in(&temp.0, &["add", "."]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "first"]);
    temp.write("new.rs", "fn added() {}\n");
    fs::remove_file(temp.0.join("gone.rs")).unwrap();
    git_in(&temp.0, &["add", "-A"]);

    let found = changes::collect(&temp.0, &changes::Range::Staged, &[]).unwrap();
    let by = |name: &str| found.iter().find(|c| c.path == name).unwrap();
    assert_eq!(by("new.rs").kind, changes::Kind::Added);
    assert!(by("new.rs").before.is_empty());
    assert!(by("new.rs").reviewable());
    assert_eq!(by("gone.rs").kind, changes::Kind::Deleted);
    assert!(!by("gone.rs").reviewable(), "a deletion introduces nothing");
}

#[test]
fn naming_a_directory_reviews_only_the_changes_inside_it() {
    let temp = repository();
    temp.write("src/a.rs", "fn a() {}\n");
    temp.write("other/b.rs", "fn b() {}\n");
    git_in(&temp.0, &["add", "."]);
    git_in(&temp.0, &["commit", "--quiet", "-m", "first"]);
    temp.write("src/a.rs", "fn a() { one(); }\n");
    temp.write("other/b.rs", "fn b() { two(); }\n");

    let only = [PathBuf::from("src")];
    let found = changes::collect(&temp.0, &changes::Range::Unstaged, &only).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "src/a.rs");
}

#[test]
fn review_outside_a_repository_says_so() {
    let temp = Temp::new();
    let error = changes::collect(&temp.0, &changes::Range::Unstaged, &[]).unwrap_err();
    assert!(error.contains("Git repository"), "{error}");
}

#[test]
fn a_band_says_only_what_the_resolution_supports() {
    assert_eq!(band(Some(9.1)), "good");
    assert_eq!(band(Some(8.0)), "good");
    assert_eq!(band(Some(7.9)), "fair");
    assert_eq!(band(Some(5.0)), "fair");
    assert_eq!(band(Some(4.9)), "weak");
    assert_eq!(band(None), "\u{2014}");
}

fn windowed(path: &str, checks: &[(&str, f64)]) -> Answers {
    Answers {
        path: path.to_owned(),
        bytes: 100,
        windows: 1,
        note: Some("windowed".into()),
        values: Some(checks.iter().map(|(k, v)| ((*k).to_owned(), *v)).collect()),
        cached: true,
        error: None,
    }
}

#[test]
fn windows_of_one_file_fold_back_by_taking_the_strongest_answer() {
    // Every question is existential: "does this file contain a method that..."
    // is true if any window has one, so the highest value wins.
    let folded = combine_windows(vec![
        windowed("a.rs", &[("long_method", 0.1), ("dead_code", 0.9)]),
        windowed("a.rs", &[("long_method", 0.8), ("dead_code", 0.2)]),
        windowed("b.rs", &[("long_method", 0.3), ("dead_code", 0.3)]),
    ]);
    assert_eq!(folded.len(), 2);
    let a = &folded[0];
    assert_eq!(a.path, "a.rs");
    assert_eq!(a.windows, 2);
    assert_eq!(a.bytes, 200, "a windowed file reports its whole size");
    let values = a.values.as_ref().unwrap();
    assert_eq!(values["long_method"], 0.8);
    assert_eq!(values["dead_code"], 0.9);
    // Order of first appearance is kept, so the report is stable.
    assert_eq!(folded[1].path, "b.rs");
    assert_eq!(folded[1].windows, 1);
}

#[test]
fn folding_keeps_the_first_error_and_only_claims_cached_when_every_window_was() {
    let mut miss = windowed("a.rs", &[("long_method", 0.1)]);
    miss.cached = false;
    let folded = combine_windows(vec![windowed("a.rs", &[("long_method", 0.2)]), miss]);
    assert!(
        !folded[0].cached,
        "one live window means the file was not cached"
    );
}

#[test]
fn an_oversized_file_is_windowed_and_every_window_fits() {
    // A single file can be split where a pair of versions cannot: there is one
    // text, so each window is still real code to ask about.
    let mut source = String::from("// oversized\n");
    for index in 0..3000 {
        source.push_str(&format!(
            "export function unit{index}(a: number) {{\n  return a + {index};\n}}\n"
        ));
    }
    assert!(
        within_budget(&smells::file_request("big.ts", &source))
            .unwrap()
            .is_none(),
        "the fixture must exceed the budget or this proves nothing"
    );
    let windows = catalog_windows("big.ts", &source).unwrap();
    assert!(windows.len() > 1, "an oversized file is split");
    for (window, text) in &windows {
        assert!(
            within_budget(&smells::file_request("big.ts", text))
                .unwrap()
                .is_some(),
            "window {} of {} is still over budget",
            window.index,
            window.of
        );
    }
    // The windows partition the file: every byte is asked about exactly once.
    let covered: usize = windows.iter().map(|(_, text)| text.len()).sum();
    assert_eq!(covered, source.len());
    assert_eq!(windows.last().unwrap().0.of, windows.len());
}

#[test]
fn a_file_with_nothing_to_window_on_says_so_rather_than_failing_silently() {
    let source = format!("const blob = \"{}\";\n", "x".repeat(200_000));
    let error = catalog_windows("blob.ts", &source).unwrap_err();
    assert!(
        error.contains("no parsed top-level declarations"),
        "{error}"
    );
}
