//! The shape of every file a run keeps, worked out from the types that write
//! and read it, and held against `tests/compat/store-shapes.txt`.
//!
//! A run outlives the release that wrote it. 4.24.0 renamed the field that
//! names a decision in evidence (`meta` to `id`) and kept the evidence's
//! schema version, and every run recorded before it stopped reading. Nothing
//! said so when the field was renamed: the backward-compatibility corpus
//! (`crates/supercov-cli/tests/compat.rs`) found it only once runs of the
//! release before were recorded and read.
//!
//! This test says so as the change is made. Each stored file is listed with
//! the version that names its shape and the types that write and read it.
//! Their shapes come from the types themselves (`schemars`, which reads
//! serde's attributes: renames, defaults, skipped fields, `try_from` wire
//! types), once as serialized and once as deserialized, and are written as
//! plain text a reviewer can read in a diff. A change to any of them fails
//! here until the snapshot changes with it, and the failure says what else
//! the change owes the runs already written.

use std::collections::BTreeMap;

use schemars::{
    JsonSchema, SchemaGenerator,
    generate::{Contract, SchemaSettings},
};
use serde_json::Value;

/// The snapshot, beside the runs earlier releases recorded.
fn snapshot_path() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/compat/store-shapes.txt")
}

/// Set to write the snapshot from the types instead of checking it.
const UPDATE: &str = "SUPERCOV_UPDATE_STORE_SHAPES";

/// One file a run keeps: where it is, the version that names its shape, and
/// the shape of what is written there and read from it.
struct Stored {
    file: &'static str,
    version: Option<(&'static str, u32)>,
    shape: fn(&mut SchemaGenerator) -> Value,
    /// Only read: something other than Supercov's Rust writes it (the
    /// JavaScript runtime's launch observer).
    read_only: bool,
}

fn shape<T: JsonSchema>(generator: &mut SchemaGenerator) -> Value {
    generator.subschema_for::<T>().to_value()
}

/// Every file a run keeps that a type writes or reads, in the order a run
/// gets them.
fn stored() -> Vec<Stored> {
    use crate::coverage_report::{
        CoverageManifest, ExecutionTraceEvent, PersistedCoverageModel, RawTestResult, ServerRecord,
    };
    use supercov_contracts::{
        COVERAGE_MODEL_SCHEMA_VERSION, EVIDENCE_ARCHIVE_SCHEMA_VERSION, FrontendRunDeclaration,
        LANGUAGE_FRONTEND_PROTOCOL_VERSION,
    };
    let evidence = Some((
        "EVIDENCE_ARCHIVE_SCHEMA_VERSION",
        EVIDENCE_ARCHIVE_SCHEMA_VERSION,
    ));
    let sources = Some((
        "source_manifest::MANIFEST_SCHEMA",
        crate::source_manifest::MANIFEST_SCHEMA,
    ));
    vec![
        Stored {
            file: "run.json",
            version: Some((
                "RUN_INTEGRITY_SCHEMA_VERSION",
                crate::integrity::RUN_INTEGRITY_SCHEMA_VERSION,
            )),
            shape: shape::<crate::run_store::RunMetadata>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: the header before each entry",
            version: evidence,
            shape: shape::<crate::evidence_archive::EntryHeader>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: frontend.json",
            version: Some((
                "LANGUAGE_FRONTEND_PROTOCOL_VERSION",
                LANGUAGE_FRONTEND_PROTOCOL_VERSION,
            )),
            shape: shape::<FrontendRunDeclaration>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: coverage-model.json",
            version: Some((
                "COVERAGE_MODEL_SCHEMA_VERSION",
                COVERAGE_MODEL_SCHEMA_VERSION,
            )),
            shape: shape::<PersistedCoverageModel>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: manifest.json",
            version: evidence,
            shape: shape::<CoverageManifest>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: mcdc.json, and each line of *.mcdc.jsonl",
            version: evidence,
            shape: shape::<RawTestResult>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: each line of server/**/*.jsonl",
            version: evidence,
            shape: shape::<ServerRecord>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: each line of execution.*.jsonl",
            version: evidence,
            shape: shape::<ExecutionTraceEvent>,
            read_only: true,
        },
        Stored {
            file: "evidence.raw.gz: source-inputs.json",
            version: sources,
            shape: shape::<crate::source_manifest::InputManifest>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: statement-exclusions.json",
            version: evidence,
            shape: shape::<Vec<Value>>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: support-reach.json",
            version: evidence,
            shape: shape::<crate::support_reach::SupportReach>,
            read_only: false,
        },
        Stored {
            file: "evidence.raw.gz: mutants.json; mutants-listed.json beside it",
            version: Some((
                "mutant_list::MUTANTS_SCHEMA_VERSION",
                crate::mutant_list::MUTANTS_SCHEMA_VERSION,
            )),
            shape: shape::<crate::mutant_list::MutantsFile>,
            read_only: false,
        },
        Stored {
            file: "source-manifest.cache.json",
            version: sources,
            shape: shape::<crate::source_manifest::ManifestCache>,
            read_only: false,
        },
        Stored {
            file: "test-executions.json",
            version: None,
            shape: shape::<crate::source_manifest::Executions>,
            read_only: false,
        },
        Stored {
            file: "statement-spans.json",
            version: None,
            shape: shape::<Vec<crate::run_store::StatementSpan>>,
            read_only: false,
        },
        Stored {
            file: "assessment-input.json.gz",
            version: None,
            shape: shape::<crate::assertion_coverage::AssessmentInput>,
            read_only: false,
        },
        Stored {
            file: "strong-mutation.json",
            version: Some((
                "strong_plan::STRONG_VERDICTS_SCHEMA_VERSION",
                crate::strong_plan::STRONG_VERDICTS_SCHEMA_VERSION,
            )),
            shape: shape::<crate::strong_plan::StrongVerdicts>,
            read_only: false,
        },
        Stored {
            file: "query-index.v1.bin: the identity in its header",
            version: Some((
                "QUERY_INDEX_SCHEMA_VERSION",
                crate::query_index::QUERY_INDEX_SCHEMA_VERSION,
            )),
            shape: shape::<crate::query_index::QueryIndexIdentity>,
            read_only: false,
        },
    ]
}

/// The snapshot's text: each file, then each type its shapes name.
fn render() -> String {
    let files = stored();
    let mut generators = [Contract::Serialize, Contract::Deserialize].map(|contract| {
        SchemaSettings::draft2020_12()
            .with(|settings| settings.contract = contract)
            .into_generator()
    });
    let mut out = String::from(HEADER);
    for file in &files {
        let [written, read] = &mut generators;
        let written = (!file.read_only).then(|| expression(&(file.shape)(written), "  "));
        let read = expression(&(file.shape)(read), "  ");
        out.push_str(&format!("\nfile {}\n", file.file));
        match file.version {
            Some((name, value)) => out.push_str(&format!("  version: {name} = {value}\n")),
            None => out.push_str("  version: none\n"),
        }
        let Some(written) = written else {
            out.push_str(&format!("  read: {read}\n"));
            continue;
        };
        if written == read {
            out.push_str(&format!("  written and read: {written}\n"));
        } else {
            out.push_str(&format!("  written: {written}\n  read: {read}\n"));
        }
    }
    let [written, read] = generators.map(|mut generator| generator.take_definitions(true));
    let names = written
        .keys()
        .chain(read.keys())
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    for name in names {
        let shown = |definitions: &serde_json::Map<String, Value>| {
            definitions.get(&name).map(|schema| expression(schema, ""))
        };
        match (shown(&written), shown(&read)) {
            (Some(written), Some(read)) if written == read => {
                out.push_str(&format!(
                    "\ntype {name}, written and read ={}\n",
                    spaced(&written)
                ));
            }
            (written, read) => {
                for (how, shape) in [("written", written), ("read", read)] {
                    if let Some(shape) = shape {
                        out.push_str(&format!("\ntype {name}, {how} ={}\n", spaced(&shape)));
                    }
                }
            }
        }
    }
    out
}

/// A shape after `=`: on the same line, or on the lines below.
fn spaced(shape: &str) -> String {
    if shape.starts_with('\n') {
        shape.to_owned()
    } else {
        format!(" {shape}")
    }
}

const HEADER: &str = "\
# The shape of every file a run keeps, from the types that write and read it.
# crates/supercov-engine/src/store_shapes.rs checks this against the types.
#
# `field?` may be left out: a field written only when it has a value, or read
# with a default. `closed` refuses fields it does not know. `uint` is an
# integer that is never negative. A type is shown once where what is written
# and what is read are the same, and once each where they differ.
";

/// A schema as one line of text, or a block indented under `indent` where
/// it has fields or alternatives of its own.
fn expression(schema: &Value, indent: &str) -> String {
    let map = match schema {
        Value::Bool(true) => return "any".into(),
        Value::Bool(false) => return "nothing".into(),
        Value::Object(map) => map,
        other => panic!("not a schema: {other}"),
    };
    if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
        return reference.rsplit('/').next().unwrap_or(reference).to_owned();
    }
    if let Some(constant) = map.get("const") {
        return constant.to_string();
    }
    if let Some(values) = map.get("enum").and_then(Value::as_array) {
        return values
            .iter()
            .map(Value::to_string)
            .collect::<Vec<_>>()
            .join(" | ");
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(alternatives) = map.get(key).and_then(Value::as_array) {
            return alternatives_of(alternatives, indent);
        }
    }
    if let Some(parts) = map.get("allOf").and_then(Value::as_array) {
        return parts
            .iter()
            .map(|part| expression(part, indent))
            .collect::<Vec<_>>()
            .join(" & ");
    }
    let types = match map.get("type") {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        None if map.contains_key("properties") => vec!["object"],
        None => return "any".into(),
        Some(other) => panic!("not a type: {other}"),
    };
    types
        .into_iter()
        .map(|kind| match kind {
            "object" => object(map, indent),
            "array" => array(map, indent),
            "integer" if map.get("minimum").and_then(Value::as_f64) == Some(0.0) => "uint".into(),
            "integer" => "int".into(),
            other => other.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Alternatives on one line, or one under another where any is a block.
fn alternatives_of(alternatives: &[Value], indent: &str) -> String {
    let inner = format!("{indent}  ");
    let shown = alternatives
        .iter()
        .map(|alternative| expression(alternative, &inner))
        .collect::<Vec<_>>();
    if shown.iter().any(|alternative| alternative.contains('\n')) {
        shown
            .iter()
            .map(|alternative| format!("\n{indent}| {alternative}"))
            .collect()
    } else {
        shown.join(" | ")
    }
}

fn object(map: &serde_json::Map<String, Value>, indent: &str) -> String {
    let closed = map.get("additionalProperties") == Some(&Value::Bool(false));
    let Some(properties) = map.get("properties").and_then(Value::as_object) else {
        if let Some(values @ Value::Object(_)) = map.get("additionalProperties") {
            return format!("{{string: {}}}", expression(values, indent));
        }
        // A map whose keys are numbers.
        if let Some(patterns) = map.get("patternProperties").and_then(Value::as_object)
            && let [(pattern, values)] = patterns.iter().collect::<Vec<_>>()[..]
        {
            let key = if pattern == "^\\d+$" { "uint" } else { pattern };
            return format!("{{{key}: {}}}", expression(values, indent));
        }
        return "object".into();
    };
    let required = map
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect::<Vec<_>>())
        .unwrap_or_default();
    let inner = format!("{indent}  ");
    let mut text = String::from(if closed { "closed {\n" } else { "{\n" });
    for (name, field) in properties {
        let optional = if required.contains(&name.as_str()) {
            ""
        } else {
            "?"
        };
        text.push_str(&format!(
            "{inner}{name}{optional}: {}\n",
            expression(field, &inner)
        ));
    }
    if let Some(values @ Value::Object(_)) = map.get("additionalProperties") {
        text.push_str(&format!("{inner}..: {}\n", expression(values, &inner)));
    }
    text.push_str(indent);
    text.push('}');
    text
}

fn array(map: &serde_json::Map<String, Value>, indent: &str) -> String {
    if let Some(items) = map.get("prefixItems").and_then(Value::as_array) {
        let items = items
            .iter()
            .map(|item| expression(item, indent))
            .collect::<Vec<_>>();
        return format!("({})", items.join(", "));
    }
    let item = map
        .get("items")
        .map_or_else(|| "any".to_owned(), |item| expression(item, indent));
    match (map.get("minItems"), map.get("maxItems")) {
        (Some(least), Some(most)) if least == most => format!("[{item}; {least}]"),
        _ => format!("[{item}]"),
    }
}

/// The lines of `before` and `after` that differ, in order, each under the
/// `file` or `type` it belongs to.
fn changed_lines(before: &str, after: &str) -> String {
    let before = before.lines().collect::<Vec<_>>();
    let after = after.lines().collect::<Vec<_>>();
    // Longest common subsequence, by lengths from the end.
    let mut common = vec![vec![0_usize; after.len() + 1]; before.len() + 1];
    for i in (0..before.len()).rev() {
        for j in (0..after.len()).rev() {
            common[i][j] = if before[i] == after[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let heading = |line: &str| line.starts_with("file ") || line.starts_with("type ");
    let (mut i, mut j) = (0, 0);
    let mut under = "";
    let mut shown = String::new();
    let mut out = String::new();
    let mut show = |out: &mut String, under: &str, line: String| {
        // The blank lines between entries say nothing.
        if line.trim().len() == 1 {
            return;
        }
        if under != shown && !heading(&line[2..]) {
            out.push_str(&format!("  {under}\n"));
        }
        under.clone_into(&mut shown);
        out.push_str(&line);
    };
    while i < before.len() || j < after.len() {
        if i < before.len() && j < after.len() && before[i] == after[j] {
            if heading(before[i]) {
                under = before[i];
            }
            i += 1;
            j += 1;
        } else if i < before.len() && (j == after.len() || common[i + 1][j] >= common[i][j + 1]) {
            if heading(before[i]) {
                under = before[i];
            }
            show(&mut out, under, format!("- {}\n", before[i]));
            i += 1;
        } else {
            if heading(after[j]) {
                under = after[j];
            }
            show(&mut out, under, format!("+ {}\n", after[j]));
            j += 1;
        }
    }
    out
}

#[test]
fn what_runs_store_has_the_shape_the_snapshot_records() {
    let current = render();
    let path = snapshot_path();
    if std::env::var_os(UPDATE).is_some() {
        std::fs::write(&path, &current).unwrap();
        return;
    }
    let recorded = std::fs::read_to_string(&path).unwrap_or_default();
    // Line endings as a Windows checkout may have them.
    let recorded = recorded.replace("\r\n", "\n");
    assert!(
        recorded == current,
        "\nThe shape of what a run stores changed (- the snapshot, + the types):\n\n{}\n\
         Runs written before this change hold the old shape, and every later release must \
         still read them. In the same change:\n\
         \n\
         1. Bump the version of each file that holds the changed type (its `version`), and have \
         this build convert a run of the old version, or refuse it with a message that says so. Or keep reading the \
         old shape: a `#[serde(default)]` or `alias`, or a wire type the stored one converts \
         from with `try_from`, as `ServerRecordWire` reads decisions named in `meta` before \
         4.24.0.\n\
         2. Make sure the backward-compatibility corpus holds a run in the old shape: record the \
         last release that wrote it, for a fixture that makes it write that file \
         (scripts/compat-record.sh <fixture> <version>; tests/compat/README.md), and run \
         `cargo test -p supercov --test compat`.\n\
         3. Write the new shapes into {}:\n\
         \n    {UPDATE}=1 cargo test -p supercov-engine --lib store_shapes\n",
        changed_lines(&recorded, &current),
        "tests/compat/store-shapes.txt",
    );
}

#[test]
fn a_changed_line_is_shown_as_removed_and_added() {
    assert_eq!(
        changed_lines("a\nb\nc\n", "a\nB\nc\nd\n"),
        "- b\n+ B\n+ d\n"
    );
    // Each change under the type it is in, named once.
    assert_eq!(
        changed_lines(
            "type A = {\n  x: int\n}\n\ntype B = {\n  y: int\n  z: int\n}\n",
            "type A = {\n  x: int\n}\n\ntype B = {\n  w: int\n  z: uint\n}\n",
        ),
        "  type B = {\n-   y: int\n-   z: int\n+   w: int\n+   z: uint\n"
    );
    assert_eq!(
        changed_lines("type A = int\n", "type A = int\n\ntype C = string\n"),
        "+ type C = string\n"
    );
    assert_eq!(changed_lines("same\n", "same\n"), "");
}

#[test]
fn shapes_read_as_the_types_say() {
    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    #[allow(dead_code)]
    struct Example {
        renamed_field: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maybe: Option<i64>,
        pair: [i64; 2],
        map: BTreeMap<String, Vec<String>>,
    }
    let mut generator = SchemaSettings::draft2020_12().into_generator();
    let shown = expression(&shape::<Example>(&mut generator), "");
    assert_eq!(shown, "Example");
    let definitions = generator.take_definitions(true);
    assert_eq!(
        expression(&definitions["Example"], ""),
        "closed {\n  renamedField: uint\n  maybe?: int | null\n  pair: [int; 2]\n  map: {string: [string]}\n}"
    );
}
